//! The position seam: session-canonical position identity, the refusal
//! vocabulary a position read can answer with, and the trait a lending
//! integration implements to serve one.
//!
//! # A position is not a session object — its identity is
//!
//! A session object is something the session *names* and hands out by
//! reference. A position is not that: the value decays while a caller holds it,
//! so a session-cached one would hide freshness from a strategy that is about
//! to act on it, and a get-or-create verb would have to invent the value it
//! could not read — turning a transient read failure into a fabricated
//! position. What IS session-canonical is the position's **identity**:
//! `(chain, market, account)`, a typed key with no value attached.
//!
//! # The value is a fresh read-model projection
//!
//! [`PositionReading`] carries the observation's block, so a caller can say how
//! fresh it needs ([`Freshness`]) and get [`PositionRefusal::StaleObservation`]
//! rather than a decaying snapshot dressed up as current. The family-specific
//! detail of a position (asset lists, e-mode, isolation limits) stays with the
//! family's own types: a strategy that needs it reads it from the family
//! directly, and this module stays family-agnostic so the session never learns
//! what a lending market is.
//!
//! # Why the seam sits in `degenbot-core`
//!
//! An observer is implemented by a lending *integration*
//! (`degenbot-aave`), which must not depend on the engine, and the engine must
//! not depend on an integration either. `degenbot-core` is the one layer both
//! already depend on, so the seam lives here: any layer above it would need a
//! new edge in one direction or the other, and neither the re-export facade nor
//! the `PyO3` shell is a legitimate home for a domain adapter. The session
//! reaches the trait from its own object registry
//! (`degenbot_bot::bot_core::session_registry`); the integration implements it
//! in its own crate with no new edge.
//!
//! # No store, no default
//!
//! The module holds no collection and no cached value: the registry keeps a
//! *handle* to one observer, and a read that fails is a typed refusal with
//! nothing attached to it. A caller that wants a position held across a solve
//! caches it as its own derived value.

use std::fmt;

use alloy::primitives::{Address, U256};

/// Canonical identity of a position: the chain, the lending market (pool)
/// contract, and the account whose position it is.
///
/// The market is named by its pool contract rather than by a family-specific
/// handle, so the same key names an Aave market, a Morpho market, or any other
/// lending market without the session learning what a market is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PositionIdentity {
    chain_id: u64,
    market: Address,
    account: Address,
}

impl PositionIdentity {
    /// The position of `account` in the `market` pool contract on `chain_id`.
    #[must_use]
    pub const fn new(chain_id: u64, market: Address, account: Address) -> Self {
        Self {
            chain_id,
            market,
            account,
        }
    }

    /// The chain this position lives on.
    #[must_use]
    pub const fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// The lending market (pool) contract this position is in.
    #[must_use]
    pub const fn market(&self) -> Address {
        self.market
    }

    /// The account that holds the position.
    #[must_use]
    pub const fn account(&self) -> Address {
        self.account
    }
}

impl fmt::Display for PositionIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "position of {} in market {} on chain {}",
            self.account, self.market, self.chain_id
        )
    }
}

/// How fresh a reading must be for a caller to act on it.
///
/// Stated by the caller rather than defaulted by the reader, because "how old
/// is too old" is a decision about what the caller is about to DO — a
/// liquidation threshold and a risk report have different answers. `Any` is an
/// explicit request for no freshness guarantee; it is not the absence of one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freshness {
    /// Any observation, however old.
    Any,
    /// The observation must have been made at `block` or later.
    AtOrAfter {
        /// The earliest acceptable observation block.
        block: u64,
    },
    /// The observation must be no more than `max_age` blocks behind `head`.
    AtMost {
        /// How many blocks of lag the caller tolerates.
        max_age: u64,
        /// The caller's current head.
        head: u64,
    },
}

impl Freshness {
    /// Whether an observation taken at `observed_block` satisfies this
    /// requirement. A source that has never been advanced reports `None` and
    /// satisfies nothing but [`Freshness::Any`]: no observation is stale in the
    /// strongest sense, and reporting a block for it would be a fabrication.
    #[must_use]
    pub fn accepts(self, observed_block: Option<u64>) -> bool {
        match self {
            Self::Any => true,
            Self::AtOrAfter { block } => observed_block.is_some_and(|observed| observed >= block),
            Self::AtMost { max_age, head } => {
                observed_block.is_some_and(|observed| observed >= head.saturating_sub(max_age))
            }
        }
    }
}

impl fmt::Display for Freshness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Any => f.write_str("any age"),
            Self::AtOrAfter { block } => write!(f, "at or after block {block}"),
            Self::AtMost { max_age, head } => {
                write!(f, "at most {max_age} blocks behind head {head}")
            }
        }
    }
}

/// A position's liquidation posture, in the one form every lending family can
/// state: a fixed-point ratio where `1e18` is the protocol's own liquidation
/// threshold, or "there is no debt".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthFactor {
    /// The account has no debt, so there is no finite health factor.
    ///
    /// A distinct variant rather than a sentinel number: a zero health factor
    /// means LIQUIDATABLE, so a reader that reported "no debt" as zero would
    /// fabricate a liquidation out of an empty position.
    NoDebt,
    /// The finite health factor, scaled by `1e18` (`1e18` is the threshold).
    Ratio(U256),
}

impl HealthFactor {
    /// Whether the position is liquidatable at the ratio the family itself uses
    /// as its liquidation threshold. [`HealthFactor::NoDebt`] never is.
    #[must_use]
    pub fn is_liquidatable(self) -> bool {
        match self {
            Self::NoDebt => false,
            Self::Ratio(ratio) => ratio < U256::from(1_000_000_000_000_000_000u64),
        }
    }
}

/// One fresh reading of a position: who it is for, the block it was observed
/// at, and the posture that block reported.
///
/// Not a session object and not a snapshot to hold: the observation block is
/// what makes the reading perishable, which is why a caller states the
/// freshness it needs and why the registry hands these back one read at a time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PositionReading {
    identity: PositionIdentity,
    observed_block: u64,
    health_factor: HealthFactor,
}

impl PositionReading {
    /// The reading an observer produced for `identity` from `observed_block`.
    /// Public because the observer side of [`PositionObserver`] is the only
    /// legitimate constructor; a consumer never mints its own.
    #[must_use]
    pub const fn new(
        identity: PositionIdentity,
        observed_block: u64,
        health_factor: HealthFactor,
    ) -> Self {
        Self {
            identity,
            observed_block,
            health_factor,
        }
    }

    /// The position this reading is about.
    #[must_use]
    pub const fn identity(&self) -> &PositionIdentity {
        &self.identity
    }

    /// The block the observation was taken at — the reading's own age, so a
    /// caller never has to guess it.
    #[must_use]
    pub const fn observed_block(&self) -> u64 {
        self.observed_block
    }

    /// The liquidation posture at `observed_block`.
    #[must_use]
    pub const fn health_factor(&self) -> HealthFactor {
        self.health_factor
    }
}

/// Why a position read was refused.
///
/// Typed by cause so a caller branches without parsing a message, and split by
/// whether a retry could help ([`Self::is_retryable`]). Every variant is a
/// refusal and NOTHING else: a failed read has no value attached, because the
/// two ways to get that wrong — serving a remembered reading as current and
/// inventing one on a failed read — are the same defect seen from either side.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PositionRefusal {
    /// No observer is installed on the session, so it cannot answer for any
    /// position. Distinct from every other refusal: the session is unwired,
    /// not the market or the account.
    #[error("no position observer is installed on this session")]
    NoPositionOwner,
    /// The identity names a chain this session is not scoped to. The chain is
    /// part of the key, so the session refuses before asking an observer that
    /// cannot answer for it.
    #[error("{identity} is not on this session's chain {session_chain_id}")]
    ChainScopeMismatch {
        /// The identity the request named.
        identity: PositionIdentity,
        /// The chain this session is scoped to.
        session_chain_id: u64,
    },
    /// No observer is configured for the market the identity names — a market
    /// that is unknown, deactivated, or served by a different observer.
    #[error("no position observer serves {identity}")]
    MarketNotServed {
        /// The identity the request named.
        identity: PositionIdentity,
    },
    /// The market is served and the account is readable, but it holds no
    /// position there. An empty position is a fact about the account, not a
    /// failure to read one.
    #[error("{identity} holds no position in a served market")]
    UnknownPosition {
        /// The identity the request named.
        identity: PositionIdentity,
    },
    /// The read failed in a way a retry may resolve (a contended source, a
    /// transport fault). The one refusal a caller should retry.
    #[error("reading {identity} failed and may be retried: {reason}")]
    TransientRead {
        /// The identity the request named.
        identity: PositionIdentity,
        /// The failure, verbatim.
        reason: String,
    },
    /// The source rows were read but cannot be interpreted (a malformed stored
    /// value, a math overflow). Retrying unchanged reproduces it.
    #[error("reading {identity} produced rows that cannot be interpreted: {reason}")]
    UnreadablePosition {
        /// The identity the request named.
        identity: PositionIdentity,
        /// The failure, verbatim.
        reason: String,
    },
    /// The observation is older than the freshness the caller required, so it
    /// is refused rather than returned as if it were current. `observed_block`
    /// is `None` when the source has never been advanced at all.
    #[error("the reading for {identity} is too old ({observed_block:?}, required {required})")]
    StaleObservation {
        /// The identity the request named.
        identity: PositionIdentity,
        /// The block the reading was observed at, if it was observed at all.
        observed_block: Option<u64>,
        /// The freshness the caller required.
        required: Freshness,
    },
}

impl PositionRefusal {
    /// Whether reading again could plausibly produce a value.
    ///
    /// Only [`Self::TransientRead`] is retryable. A stale observation, an
    /// absent position, an unserved market, and a mis-scoped chain are all
    /// facts about the request or the source, and retrying them unchanged just
    /// spends the caller's time to learn the same thing again.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(self, Self::TransientRead { .. })
    }
}

/// The session's side of the boundary with whoever reads positions.
///
/// A session registry holds a *handle* to one observer rather than a map of
/// positions, so the implementation — the integration that owns the lending
/// domain and its I/O — is reached through this trait and never named by the
/// session. An implementation MUST answer a failed read with a refusal: it may
/// not return a value it constructed, defaulted, or remembered, because a
/// caller about to act on a position cannot tell a fabricated one from a real
/// one.
pub trait PositionObserver: Send + Sync {
    /// Read `identity` at least as fresh as `freshness` requires.
    ///
    /// An implementation may refuse as stale on its own when it knows it
    /// cannot produce a fresh-enough observation, and refusing early is
    /// cheaper than reading; it must not do the opposite and pass a
    /// too-old observation back as current, because the session also checks
    /// the reading it gets.
    ///
    /// # Errors
    ///
    /// A typed [`PositionRefusal`]. Never a value alongside a fault.
    fn read_position(
        &self,
        identity: &PositionIdentity,
        freshness: &Freshness,
    ) -> Result<PositionReading, PositionRefusal>;
}
