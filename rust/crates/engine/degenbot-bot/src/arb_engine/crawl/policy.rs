//! The crawl's path-composition policy, as data.
//!
//! `PathPolicy` (`arbitrage/policy.py`) carries its rules and a caller-supplied
//! liquidity extractor as Python callables. The core cannot hold a callable, so
//! the same policy lands here as a value whose every rule the core evaluates
//! itself, and a refusal is a typed value instead of a raised exception.
//!
//! Rule order is `policy.py:190-231`'s: hop-count (min then max), the
//! touched-token denylist/allowlist, the duplicate-pool guard, then the
//! liquidity floor. The FIRST violation is the verdict — callers classify
//! refusals by variant, so the order is part of the contract.

use std::collections::BTreeSet;

use alloy::primitives::{Address, U256};
#[cfg(test)]
use degenbot_core::fee::fee_ratio;
use degenbot_pools::{PoolEntry, RegisteredPoolFamily};
use degenbot_substrate::BotState;

/// The declarative path-composition policy: `PathPolicy` as a value.
///
/// Every rule is opt-in; [`CrawlPathPolicy::default`] admits every path (the
/// Python `NoOpPathPredicate`, or an all-default `PathPolicy`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CrawlPathPolicy {
    /// Hop-count bounds: min first, then max (`policy.py:165-166`).
    pub hop_bounds: HopBounds,
    /// Touched-token denylist / allowlist (`policy.py:161-162`).
    pub tokens: TokenFilter,
    /// Whether a repeated pool identity refuses the path (`policy.py:169`).
    pub duplicate_pools: DuplicatePoolPolicy,
    /// `None` = the floor is off. Python's two conditions (`liquidity_of is
    /// None` and `min_liquidity == 0`) are one here: a floor with no proxy is
    /// not a policy any core can evaluate (`policy.py:225`).
    pub liquidity_floor: Option<LiquidityFloor>,
}

/// The hop-count window, inclusive at both ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HopBounds {
    /// Python `min_hops`, default 1 (`policy.py:165`).
    pub min: usize,
    /// Python `max_hops`, default `sys.maxsize` (`policy.py:166`) — `None` is
    /// the same answer without the sentinel.
    pub max: Option<usize>,
}

impl Default for HopBounds {
    fn default() -> Self {
        Self { min: 1, max: None }
    }
}

/// The touched-token filter. Membership is by canonical [`Address`], so a
/// caller may spell a token in any case.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TokenFilter {
    /// Refused outright (`policy.py:161`).
    pub deny: BTreeSet<Address>,
    /// When present, a touched token outside it is refused (`policy.py:162`).
    pub allow: Option<BTreeSet<Address>>,
}

/// Whether a pool identity may repeat in one path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DuplicatePoolPolicy {
    /// Python `reject_duplicate_pools = True` (the default, `policy.py:169`).
    #[default]
    Reject,
    /// Python `reject_duplicate_pools = False`.
    Allow,
}

/// Which per-pool liquidity proxy the floor compares.
///
/// Python's `PathPolicy.liquidity_of` is a callable, so the core names the
/// proxies it can compute itself and refuses anything else at configure time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiquidityProxy {
    /// V2/Aerodrome: `reserve0 + reserve1` (the shape
    /// `test_path_policy.py:245` uses).
    ReserveSum,
    /// V3/V4: active `liquidity`.
    ActiveLiquidity,
}

/// An active minimum-liquidity floor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiquidityFloor {
    /// The floor; a hop whose proxy reads below it is refused.
    pub min: U256,
    /// The proxy the floor reads.
    pub proxy: LiquidityProxy,
}

/// One policy violation — a closed set, one variant per `PathRejectedError`
/// subtype (`degenbot/exceptions.py`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyRefusal {
    /// `HopCountInsufficientError`.
    HopCountInsufficient { hops: usize, min: usize },
    /// `HopCountExceededError`.
    HopCountExceeded { hops: usize, max: usize },
    /// `TokenDenylistedError` — both Python cases (`deny` hit, or absent from
    /// `allow`) render through this one variant, distinguished by [`DenyWhy`].
    TokenDenylisted { token: Address, why: DenyWhy },
    /// `DuplicatePoolError`.
    DuplicatePool { pool: PoolIdentityTag },
    /// A foreign policy's own refusal (the bulk-verdict escape): the shell's
    /// predicate denied the path, rendered. Never produced by the data policy.
    Oracle { detail: String },
    /// `InsufficientLiquidityError`.
    InsufficientLiquidity {
        pool_id: u64,
        liquidity: U256,
        min: U256,
    },
}

/// Why a token was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DenyWhy {
    /// The token is on the denylist.
    Denylisted,
    /// The token is absent from a configured allowlist.
    NotAllowlisted,
}

/// The identity a duplicate check keys on — the core spelling of Python's
/// `_pool_identity` (`policy.py:98`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PoolIdentityTag {
    /// V2/V3/Aerodrome/…: the checksummed pool address.
    Address(Address),
    /// V4: the pool id (the `PoolManager` is not unique per pool).
    V4([u8; 32]),
}

/// The policy answer for one candidate path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathPolicyVerdict {
    /// The path passes every enabled rule.
    Admit,
    /// The first enabled rule the path violated.
    Deny { refusal: PolicyRefusal },
}

/// The full composition view — what one candidate path exposes to policy.
///
/// Built once per candidate from state already in core memory; [`CrawlPathPolicy::evaluate`]
/// reads nothing else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathCompositionView {
    /// Per hop, in hop order.
    pub hops: Vec<HopComposition>,
}

/// One hop's composition facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HopComposition {
    /// The registered pool id the hop swaps through.
    pub pool_id: u64,
    /// The structural family (the operator surface renders the same refusal
    /// text a Python caller sees).
    pub family: RegisteredPoolFamily,
    /// The identity the duplicate-pool guard keys on.
    pub identity: PoolIdentityTag,
    /// `true` → token0→token1; `false` → token1→token0.
    pub zero_for_one: bool,
    /// The direction-filtered input token (`policy.py:113`).
    pub token_in: Address,
    /// The direction-filtered output token (`policy.py:113`).
    pub token_out: Address,
    /// The configured floor's proxy reading for this hop, populated only when
    /// the policy has a floor. `None` means the configured proxy does not
    /// apply to this hop (the builder's per-family answer), so the floor does
    /// not compare it.
    pub liquidity: Option<U256>,
}

/// The state-side facts one REGISTERED pool answers — the ONE family dispatch
/// over [`PoolEntry`] that the composition view, the verify stage's identity
/// key, and the direction resolver all read.
///
/// Every field is optional per family, and a family answers only the facts its
/// state shape has: a reserve pair has reserves and per-direction fee
/// fractions, a concentrated-liquidity pool has active liquidity, and a V4 pool
/// is keyed by `(PoolManager, pool_id)` rather than an address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegisteredHopFacts {
    /// The structural family the pool registered under.
    pub family: RegisteredPoolFamily,
    /// The contract-address families' address (`None` for a V4 pool).
    pub address: Option<Address>,
    /// A V4 pool's `(PoolManager, pool_id)` (`None` for the address-keyed
    /// families).
    pub v4: Option<(Address, [u8; 32])>,
    /// The pool's token0, in on-chain order.
    pub token0: Address,
    /// The pool's token1, in on-chain order.
    pub token1: Address,
    /// A reserve pair's per-direction fee fractions as `((gamma, denom)_token0,
    /// (gamma, denom)_token1)`. An Aerodrome pair carries ONE fee for both
    /// directions, so it answers `None` rather than a fabricated asymmetric
    /// pair.
    pub fee_pair: Option<((u64, u64), (u64, u64))>,
    /// The reserve-pair proxy (`reserve0 + reserve1`), when the family has
    /// reserves.
    pub reserve_sum: Option<U256>,
    /// The active-liquidity proxy, when the family is concentrated-liquidity.
    pub active_liquidity: Option<U256>,
}

/// Read one registered pool's state-side facts.
///
/// `None` when `pool_id` is not registered, or when it names a multi-token
/// family (Curve / Balancer) whose state has no `(token0, token1)` pair. The
/// crawl builds only pair hops, so the second arm is a caller invariant
/// violation — never a policy answer (a policy cannot be asked about a path
/// that does not exist).
#[must_use]
pub fn registered_hop_facts(state: &BotState, pool_id: u64) -> Option<RegisteredHopFacts> {
    let entry = state.pool_entry(pool_id)?;
    let mut facts = RegisteredHopFacts {
        family: entry.registered_family(),
        address: None,
        v4: None,
        token0: Address::ZERO,
        token1: Address::ZERO,
        fee_pair: None,
        reserve_sum: None,
        active_liquidity: None,
    };
    match entry {
        PoolEntry::V2(pool) => {
            facts.address = Some(pool.0.address);
            facts.token0 = pool.0.token0;
            facts.token1 = pool.0.token1;
            facts.fee_pair = Some((pool.0.fee_token0.as_pair(), pool.0.fee_token1.as_pair()));
            facts.reserve_sum = Some(U256::from(pool.1.reserve0) + U256::from(pool.1.reserve1));
        }
        PoolEntry::AerodromeV2(pool) => {
            facts.address = Some(pool.0.address);
            facts.token0 = pool.0.token0;
            facts.token1 = pool.0.token1;
            facts.reserve_sum = Some(U256::from(pool.1.reserve0) + U256::from(pool.1.reserve1));
        }
        PoolEntry::V3(pool) => {
            facts.address = Some(pool.0.address);
            facts.token0 = pool.0.token0;
            facts.token1 = pool.0.token1;
            facts.active_liquidity = Some(U256::from(pool.1.liquidity));
        }
        PoolEntry::V4(pool) => {
            facts.v4 = Some((pool.0.pool_manager, pool.0.pool_id));
            facts.token0 = pool.0.pool_key.currency0;
            facts.token1 = pool.0.pool_key.currency1;
            facts.active_liquidity = Some(U256::from(pool.1.liquidity));
        }
        PoolEntry::Curve(_) | PoolEntry::BalancerWeighted(_) | PoolEntry::BalancerStable(_) => {
            return None
        }
    }
    Some(facts)
}

impl RegisteredHopFacts {
    /// The identity the direction resolver and the operator surface render for
    /// this hop.
    #[must_use]
    pub fn identity_label(&self) -> String {
        match self.v4 {
            Some((_, pool_id)) => format!("v4:0x{}", alloy::hex::encode(pool_id)),
            None => self.address.unwrap_or(Address::ZERO).to_string(),
        }
    }

    /// The configured liquidity proxy's reading for this hop; `None` when the
    /// proxy does not apply to the family (see [`LiquidityProxy`]).
    #[must_use]
    pub const fn liquidity(&self, proxy: LiquidityProxy) -> Option<U256> {
        match proxy {
            LiquidityProxy::ReserveSum => self.reserve_sum,
            LiquidityProxy::ActiveLiquidity => self.active_liquidity,
        }
    }
}

impl HopComposition {
    /// Build one hop's composition from the registered pool it names.
    ///
    /// `pool_id` is the engine id the hop swap goes through and
    /// `zero_for_one` the hop's orientation, so `token_in`/`token_out` are the
    /// direction-filtered pair (`policy.py:113`).
    ///
    /// `None` means the state cannot answer a composition fact: `pool_id` is
    /// not registered, or it names a multi-token family (Curve / Balancer)
    /// whose state has no `(token0, token1)` pair. The crawl builds only pair
    /// hops, so the second arm is a caller invariant violation — never a
    /// policy answer (a policy cannot be asked about a path that does not
    /// exist).
    #[must_use]
    pub fn from_registered(
        state: &BotState,
        pool_id: u64,
        zero_for_one: bool,
        floor: Option<&LiquidityFloor>,
    ) -> Option<Self> {
        let facts = registered_hop_facts(state, pool_id)?;
        // A V4 pool's registration identity is its on-chain pool id (one
        // `PoolManager` hosts many pools, so the manager is not unique per
        // pool); every other family is keyed by its address.
        let identity = match facts.v4 {
            Some((_, pool_id)) => PoolIdentityTag::V4(pool_id),
            None => PoolIdentityTag::Address(facts.address?),
        };
        let (token_in, token_out) = if zero_for_one {
            (facts.token0, facts.token1)
        } else {
            (facts.token1, facts.token0)
        };
        Some(Self {
            pool_id,
            family: facts.family,
            identity,
            zero_for_one,
            token_in,
            token_out,
            liquidity: floor.and_then(|floor| facts.liquidity(floor.proxy)),
        })
    }
}

impl PathCompositionView {
    /// Build the view for one admitted-or-candidate hop signature.
    ///
    /// `hop_sig` is `(pool_id, zero_for_one)` per hop, in hop order — the same
    /// signature the ledger's memos key on, so a candidate's policy view and
    /// its memo identity are one value. `None` propagates
    /// [`HopComposition::from_registered`]'s answer: a hop the state cannot
    /// describe.
    #[must_use]
    pub fn from_registered(
        state: &BotState,
        hop_sig: &[(u64, bool)],
        floor: Option<&LiquidityFloor>,
    ) -> Option<Self> {
        let hops = hop_sig
            .iter()
            .map(|&(pool_id, zero_for_one)| {
                HopComposition::from_registered(state, pool_id, zero_for_one, floor)
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self { hops })
    }
}

/// The ordered, de-duplicated token addresses a path touches.
///
/// Captures the first hop's `token_in` plus every hop's `token_out` (all
/// intermediate tokens and the final token), first occurrence winning —
/// Python's `touched_tokens` (`policy.py:139`).
#[must_use]
pub fn touched_tokens(view: &PathCompositionView) -> Vec<Address> {
    let mut touched: Vec<Address> = Vec::new();
    let mut seen: BTreeSet<Address> = BTreeSet::new();
    for (idx, hop) in view.hops.iter().enumerate() {
        // The first hop's token_in is the path's input token.
        if idx == 0 && seen.insert(hop.token_in) {
            touched.push(hop.token_in);
        }
        if seen.insert(hop.token_out) {
            touched.push(hop.token_out);
        }
    }
    touched
}

impl CrawlPathPolicy {
    /// Evaluate every enabled rule in Python's order; the first violation is
    /// the verdict.
    ///
    /// A hop whose `liquidity` is `None` is not compared by the floor: the
    /// view's builder has declared the configured proxy inapplicable there.
    #[must_use]
    pub fn evaluate(&self, view: &PathCompositionView) -> PathPolicyVerdict {
        let hop_count = view.hops.len();

        // 1. Hop-count bounds.
        if hop_count < self.hop_bounds.min {
            return deny(PolicyRefusal::HopCountInsufficient {
                hops: hop_count,
                min: self.hop_bounds.min,
            });
        }
        if let Some(max) = self.hop_bounds.max {
            if hop_count > max {
                return deny(PolicyRefusal::HopCountExceeded {
                    hops: hop_count,
                    max,
                });
            }
        }

        // 2. Token denylist / allowlist (input + intermediates + final token).
        if !self.tokens.deny.is_empty() || self.tokens.allow.is_some() {
            for token in touched_tokens(view) {
                if self.tokens.deny.contains(&token) {
                    return deny(PolicyRefusal::TokenDenylisted {
                        token,
                        why: DenyWhy::Denylisted,
                    });
                }
                if self
                    .tokens
                    .allow
                    .as_ref()
                    .is_some_and(|allow| !allow.contains(&token))
                {
                    return deny(PolicyRefusal::TokenDenylisted {
                        token,
                        why: DenyWhy::NotAllowlisted,
                    });
                }
            }
        }

        // 3. Duplicate-pool guard.
        if self.duplicate_pools == DuplicatePoolPolicy::Reject {
            let mut seen: BTreeSet<PoolIdentityTag> = BTreeSet::new();
            for hop in &view.hops {
                if !seen.insert(hop.identity) {
                    return deny(PolicyRefusal::DuplicatePool { pool: hop.identity });
                }
            }
        }

        // 4. Min-liquidity floor.
        if let Some(floor) = &self.liquidity_floor {
            for hop in &view.hops {
                let Some(liquidity) = hop.liquidity else {
                    continue;
                };
                if liquidity < floor.min {
                    return deny(PolicyRefusal::InsufficientLiquidity {
                        pool_id: hop.pool_id,
                        liquidity,
                        min: floor.min,
                    });
                }
            }
        }

        PathPolicyVerdict::Admit
    }
}

fn deny(refusal: PolicyRefusal) -> PathPolicyVerdict {
    PathPolicyVerdict::Deny { refusal }
}

// ── the bulk-verdict escape ─────────────────────────────────────────────────

/// One candidate as a foreign (shell-implemented) policy sees it.
///
/// The candidate's identity is its hop signature — the same value the ledger's
/// memos key on — beside the composition view a data policy reads. A foreign
/// policy answers a verdict per candidate; it never sees a unit's interior, a
/// receipt, or the registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyCandidate {
    /// The submission sequence, i.e. discovery order — the receipt stream's
    /// spine, so a batched answer lands on the candidate it belongs to.
    pub seq: u64,
    /// The candidate's hop signature `(pool_id, zero_for_one)` per hop.
    pub hop_sig: Vec<(u64, bool)>,
    /// The composition view the core would have evaluated as data.
    pub view: PathCompositionView,
}

/// A foreign policy the core cannot evaluate as data.
///
/// The crawl consults it only for candidates that already passed the
/// direction/memo read, and only when the configuration carries one: a policy
/// expressible as a [`CrawlPathPolicy`] is data and never crosses the seam.
/// The trait is core-side and pyo3-free; the shell installs its
/// implementation (`EngineStages::set_inline_simulator` is the precedent).
///
/// A verdict list that does not answer exactly one entry per candidate is an
/// error, never a guess: the crawl refuses to attribute a verdict to the wrong
/// path.
pub trait BatchPolicyOracle: Send + Sync + std::fmt::Debug {
    /// One verdict per candidate, in the batch's order.
    ///
    /// # Errors
    ///
    /// [`OracleError`] when the foreign policy fails or answers the wrong
    /// number of verdicts. The crawl treats it as a loud abort — the same
    /// class as a fatal verify, never a counted skip.
    fn verdicts(&self, batch: &[PolicyCandidate]) -> Result<Vec<PathPolicyVerdict>, OracleError>;
}

/// Why a foreign policy could not answer.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OracleError {
    /// The oracle answered a different number of verdicts than it was asked
    /// for.
    #[error("the policy oracle answered {answered} verdicts for {asked} candidates")]
    LengthMismatch {
        /// Candidates the crawl asked about.
        asked: usize,
        /// Verdicts the oracle answered.
        answered: usize,
    },
    /// The foreign policy raised (a Python exception, rendered).
    #[error("the policy oracle failed: {detail}")]
    Refused {
        /// The failure, rendered by the shell.
        detail: String,
    },
}

/// Answer one candidate through a foreign oracle.
///
/// # Errors
///
/// [`OracleError`] — the oracle failed, or answered any number of verdicts
/// other than one.
pub fn oracle_verdict(
    oracle: &dyn BatchPolicyOracle,
    candidate: &PolicyCandidate,
) -> Result<PathPolicyVerdict, OracleError> {
    let mut verdicts = oracle.verdicts(std::slice::from_ref(candidate))?;
    match verdicts.len() {
        1 => Ok(verdicts.remove(0)),
        answered => Err(OracleError::LengthMismatch { asked: 1, answered }),
    }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "tests assert on known-valid fixtures")]
mod registered_hop_facts_tests {
    //! `from_registered` reads the composition from the ONE state owner: the
    //! family dispatch, the direction-filtered pair, the identity the duplicate
    //! guard keys on, and the liquidity proxy.

    use alloy::primitives::{address, aliases::U112, Address, B256};
    use degenbot_pools::RegisterV2PoolParams;
    use degenbot_substrate::BotState;
    use degenbot_uniswap::dex_identity::DexVariant;

    use super::*;

    const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
    const USDC: Address = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
    const POOL: Address = address!("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc");

    fn state_with_v2_pool(reserve0: u64, reserve1: u64) -> (BotState, u64) {
        let mut state = BotState::new();
        let pool_id = state
            .register_v2_pool(&RegisterV2PoolParams {
                address: POOL,
                token0: USDC,
                token1: WETH,
                reserve0: U112::from(reserve0),
                reserve1: U112::from(reserve1),
                fee_token0: fee_ratio(997, 1000),
                fee_token1: fee_ratio(997, 1000),
                factory: address!("5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"),
                deployer: address!("5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"),
                init_hash: B256::ZERO,
                update_block: 1,
                variant: DexVariant::UniswapV2,
                stable_swap: false,
            })
            .expect("the fixture registers");
        (state, pool_id)
    }

    #[test]
    fn the_view_is_built_from_registered_state() {
        let (state, pool_id) = state_with_v2_pool(1_000, 2_000);
        let view = PathCompositionView::from_registered(&state, &[(pool_id, true)], None)
            .expect("a registered pool composes");
        let hop = &view.hops[0];
        assert_eq!(hop.pool_id, pool_id);
        assert_eq!(hop.family, RegisteredPoolFamily::V2);
        assert_eq!(hop.identity, PoolIdentityTag::Address(POOL));
        assert!(hop.zero_for_one);
        assert_eq!((hop.token_in, hop.token_out), (USDC, WETH));
        assert_eq!(hop.liquidity, None, "no floor configured, no proxy read");

        // The other orientation swaps the pair.
        let reversed = PathCompositionView::from_registered(&state, &[(pool_id, false)], None)
            .expect("a registered pool composes");
        assert_eq!(
            (reversed.hops[0].token_in, reversed.hops[0].token_out),
            (WETH, USDC)
        );
    }

    #[test]
    fn the_floor_reads_the_configured_proxy_only() {
        let (state, pool_id) = state_with_v2_pool(1_000, 2_000);
        let view = PathCompositionView::from_registered(
            &state,
            &[(pool_id, true)],
            Some(&LiquidityFloor {
                min: U256::from(500u64),
                proxy: LiquidityProxy::ReserveSum,
            }),
        )
        .expect("a registered pool composes");
        assert_eq!(view.hops[0].liquidity, Some(U256::from(3_000u64)));
        assert!(matches!(
            CrawlPathPolicy {
                liquidity_floor: Some(LiquidityFloor {
                    min: U256::from(100_000u64),
                    proxy: LiquidityProxy::ReserveSum,
                }),
                ..CrawlPathPolicy::default()
            }
            .evaluate(&view),
            PathPolicyVerdict::Deny {
                refusal: PolicyRefusal::InsufficientLiquidity { pool_id: id, .. }
            } if id == pool_id
        ));

        // A concentrated-liquidity proxy does not apply to a reserve pair.
        let view = PathCompositionView::from_registered(
            &state,
            &[(pool_id, true)],
            Some(&LiquidityFloor {
                min: U256::ZERO,
                proxy: LiquidityProxy::ActiveLiquidity,
            }),
        )
        .expect("a registered pool composes");
        assert_eq!(view.hops[0].liquidity, None);
    }

    #[test]
    fn an_unregistered_pool_has_no_view() {
        let state = BotState::new();
        assert!(PathCompositionView::from_registered(&state, &[(7, true)], None).is_none());
    }
}

#[cfg(test)]
mod tests {
    use alloy::primitives::{address, b256};

    use super::*;

    // The fixture set `tests/arbitrage/test_path_policy.py` builds through
    // `tests/types/test_concrete_pool_construction.py`: a V2 USDC/WETH pool, a
    // V3 UNI/WETH pool, and a V4 USDC/WETH pool.
    const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
    const USDC: Address = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
    /// The same token spelled as a policy may spell it — the filter compares
    /// parsed addresses, so case is not part of the token's identity.
    const USDC_LOWERCASE: Address = address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
    const UNI: Address = address!("1f9840a85d5aF5bf1D1762F925BDADdC4201F984");
    const V2_POOL: Address = address!("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc");
    const V3_POOL: Address = address!("1d42064Fc4Beb5F8aAF85F4617AE8b3b5B8Bd801");
    const V4_POOL_ID_BYTES: [u8; 32] =
        b256!("4f88f7c99022eace4740c6898f59ce6a2e798a1e64ce54589720b7153eb224a7").0;

    // The state owner assigns pool ids; the policy only echoes them back.
    const V2_POOL_ID: u64 = 100;
    const V3_POOL_ID: u64 = 200;
    const V4_POOL_ID: u64 = 300;

    /// The V2 fixture's `reserves_token0 + reserves_token1`:
    /// `2_000_000 * 10**6` USDC wei + `1000 * 10**18` WETH wei.
    fn v2_reserves_sum() -> U256 {
        U256::from(2_000_000u64) * U256::from(1_000_000u64)
            + U256::from(1000u64) * U256::from(1_000_000_000_000_000_000u64)
    }

    fn v2_hop(zero_for_one: bool) -> HopComposition {
        let (token_in, token_out) = if zero_for_one {
            (USDC, WETH)
        } else {
            (WETH, USDC)
        };
        HopComposition {
            pool_id: V2_POOL_ID,
            family: RegisteredPoolFamily::V2,
            identity: PoolIdentityTag::Address(V2_POOL),
            zero_for_one,
            token_in,
            token_out,
            liquidity: Some(v2_reserves_sum()),
        }
    }

    fn v3_hop(zero_for_one: bool) -> HopComposition {
        let (token_in, token_out) = if zero_for_one {
            (UNI, WETH)
        } else {
            (WETH, UNI)
        };
        HopComposition {
            pool_id: V3_POOL_ID,
            family: RegisteredPoolFamily::V3,
            identity: PoolIdentityTag::Address(V3_POOL),
            zero_for_one,
            token_in,
            token_out,
            liquidity: Some(U256::from(1_234_567_890u64)),
        }
    }

    fn v4_hop(zero_for_one: bool) -> HopComposition {
        let (token_in, token_out) = if zero_for_one {
            (USDC, WETH)
        } else {
            (WETH, USDC)
        };
        HopComposition {
            pool_id: V4_POOL_ID,
            family: RegisteredPoolFamily::V4,
            identity: PoolIdentityTag::V4(V4_POOL_ID_BYTES),
            zero_for_one,
            token_in,
            token_out,
            liquidity: Some(U256::from(9_876_543_210u64)),
        }
    }

    fn view(hops: Vec<HopComposition>) -> PathCompositionView {
        PathCompositionView { hops }
    }

    fn deny_tokens(token: Address, why: DenyWhy) -> PathPolicyVerdict {
        PathPolicyVerdict::Deny {
            refusal: PolicyRefusal::TokenDenylisted { token, why },
        }
    }

    /// One row of the ported `test_path_policy.py` table: the same policy
    /// input, the same composition view, the same verdict (variant *and*
    /// payload).
    struct ParityCase {
        /// The `tests/arbitrage/test_path_policy.py` case this row ports.
        python_case: &'static str,
        policy: CrawlPathPolicy,
        view: PathCompositionView,
        verdict: PathPolicyVerdict,
    }

    fn admission_cases() -> Vec<ParityCase> {
        let two_hop = vec![v2_hop(true), v3_hop(false)];

        vec![
            // A default policy (and the Python NoOp predicate, which it maps
            // to) admits any path.
            ParityCase {
                python_case: "test_policy_default_accepts_any_path",
                policy: CrawlPathPolicy::default(),
                view: view(vec![v2_hop(true)]),
                verdict: PathPolicyVerdict::Admit,
            },
            ParityCase {
                python_case: "test_noop_predicate_accepts_everything",
                policy: CrawlPathPolicy::default(),
                view: view(two_hop.clone()),
                verdict: PathPolicyVerdict::Admit,
            },
        ]
    }

    fn token_cases() -> Vec<ParityCase> {
        let two_hop = vec![v2_hop(true), v3_hop(false)];

        vec![
            // A touched intermediate token on the denylist refuses the path,
            // carrying that token.
            ParityCase {
                python_case: "test_policy_token_denylist_rejects_intermediate_token",
                policy: CrawlPathPolicy {
                    tokens: TokenFilter {
                        deny: BTreeSet::from([USDC]),
                        ..TokenFilter::default()
                    },
                    ..CrawlPathPolicy::default()
                },
                view: view(two_hop.clone()),
                verdict: deny_tokens(USDC, DenyWhy::Denylisted),
            },
            // Denylist membership normalizes the address, so a lowercase
            // entry still matches the pool's checksummed token.
            ParityCase {
                python_case: "test_policy_token_denylist_case_insensitive_via_checksum",
                policy: CrawlPathPolicy {
                    tokens: TokenFilter {
                        deny: BTreeSet::from([USDC_LOWERCASE]),
                        ..TokenFilter::default()
                    },
                    ..CrawlPathPolicy::default()
                },
                view: view(vec![v2_hop(true)]),
                verdict: deny_tokens(USDC, DenyWhy::Denylisted),
            },
            // An allowlist refuses any touched token absent from it.
            ParityCase {
                python_case: "test_policy_token_allowlist_rejects_unknown_token",
                policy: CrawlPathPolicy {
                    tokens: TokenFilter {
                        allow: Some(BTreeSet::from([WETH])),
                        ..TokenFilter::default()
                    },
                    ..CrawlPathPolicy::default()
                },
                view: view(vec![v2_hop(true)]),
                verdict: deny_tokens(USDC, DenyWhy::NotAllowlisted),
            },
            ParityCase {
                python_case: "test_policy_token_allowlist_accepts_when_all_allowed",
                policy: CrawlPathPolicy {
                    tokens: TokenFilter {
                        allow: Some(BTreeSet::from([USDC, WETH])),
                        ..TokenFilter::default()
                    },
                    ..CrawlPathPolicy::default()
                },
                view: view(vec![v2_hop(true)]),
                verdict: PathPolicyVerdict::Admit,
            },
        ]
    }

    fn hop_count_cases() -> Vec<ParityCase> {
        let two_hop = vec![v2_hop(true), v3_hop(false)];

        vec![
            ParityCase {
                python_case: "test_policy_min_hops_rejects_too_short",
                policy: CrawlPathPolicy {
                    hop_bounds: HopBounds { min: 3, max: None },
                    ..CrawlPathPolicy::default()
                },
                view: view(two_hop.clone()),
                verdict: PathPolicyVerdict::Deny {
                    refusal: PolicyRefusal::HopCountInsufficient { hops: 2, min: 3 },
                },
            },
            ParityCase {
                python_case: "test_policy_max_hops_rejects_too_long",
                policy: CrawlPathPolicy {
                    hop_bounds: HopBounds {
                        min: 1,
                        max: Some(1),
                    },
                    ..CrawlPathPolicy::default()
                },
                view: view(two_hop.clone()),
                verdict: PathPolicyVerdict::Deny {
                    refusal: PolicyRefusal::HopCountExceeded { hops: 2, max: 1 },
                },
            },
        ]
    }

    fn duplicate_cases() -> Vec<ParityCase> {
        let two_hop = vec![v2_hop(true), v3_hop(false)];

        vec![
            // The same pool twice is a degenerate, non-executable cycle; V2/V3
            // key by address.
            ParityCase {
                python_case: "test_policy_duplicate_pool_rejects_same_pool_twice",
                policy: CrawlPathPolicy::default(),
                view: view(vec![v2_hop(true), v2_hop(false)]),
                verdict: PathPolicyVerdict::Deny {
                    refusal: PolicyRefusal::DuplicatePool {
                        pool: PoolIdentityTag::Address(V2_POOL),
                    },
                },
            },
            ParityCase {
                python_case: "test_policy_duplicate_pool_can_be_disabled",
                policy: CrawlPathPolicy {
                    duplicate_pools: DuplicatePoolPolicy::Allow,
                    ..CrawlPathPolicy::default()
                },
                view: view(vec![v2_hop(true), v2_hop(false)]),
                verdict: PathPolicyVerdict::Admit,
            },
            ParityCase {
                python_case: "test_policy_duplicate_pool_distinguishes_v2_and_v3_by_address",
                policy: CrawlPathPolicy::default(),
                view: view(two_hop.clone()),
                verdict: PathPolicyVerdict::Admit,
            },
        ]
    }

    fn liquidity_cases() -> Vec<ParityCase> {
        let floor = LiquidityFloor {
            min: U256::from(1_000_000_000_000_000_000_000_000_000_000u128),
            proxy: LiquidityProxy::ReserveSum,
        };

        vec![
            // The floor reads the configured proxy; below it refuses.
            ParityCase {
                python_case: "test_policy_liquidity_gate_rejects_below_minimum",
                policy: CrawlPathPolicy {
                    liquidity_floor: Some(floor),
                    ..CrawlPathPolicy::default()
                },
                view: view(vec![v2_hop(true)]),
                verdict: PathPolicyVerdict::Deny {
                    refusal: PolicyRefusal::InsufficientLiquidity {
                        pool_id: V2_POOL_ID,
                        liquidity: v2_reserves_sum(),
                        min: floor.min,
                    },
                },
            },
            // Python's `liquidity_of=None` makes the floor a no-op; the value
            // model spells that as `liquidity_floor: None`.
            ParityCase {
                python_case: "test_policy_liquidity_gate_skipped_without_extractor",
                policy: CrawlPathPolicy::default(),
                view: view(vec![v2_hop(true)]),
                verdict: PathPolicyVerdict::Admit,
            },
        ]
    }

    fn compose_cases() -> Vec<ParityCase> {
        let v2_v4 = vec![v2_hop(true), v4_hop(false)];

        vec![
            // All enabled rules run; hop-count precedes the denylist.
            ParityCase {
                python_case: "test_policy_rules_compose",
                policy: CrawlPathPolicy {
                    hop_bounds: HopBounds {
                        min: 1,
                        max: Some(1),
                    },
                    tokens: TokenFilter {
                        deny: BTreeSet::from([USDC]),
                        ..TokenFilter::default()
                    },
                    ..CrawlPathPolicy::default()
                },
                view: view(v2_v4.clone()),
                verdict: PathPolicyVerdict::Deny {
                    refusal: PolicyRefusal::HopCountExceeded { hops: 2, max: 1 },
                },
            },
            ParityCase {
                python_case: "test_policy_rules_compose",
                policy: CrawlPathPolicy {
                    tokens: TokenFilter {
                        deny: BTreeSet::from([USDC]),
                        ..TokenFilter::default()
                    },
                    ..CrawlPathPolicy::default()
                },
                view: view(v2_v4.clone()),
                verdict: deny_tokens(USDC, DenyWhy::Denylisted),
            },
        ]
    }

    fn parity_cases() -> Vec<ParityCase> {
        let mut cases = admission_cases();
        cases.extend(token_cases());
        cases.extend(hop_count_cases());
        cases.extend(duplicate_cases());
        cases.extend(liquidity_cases());
        cases.extend(compose_cases());
        cases
    }

    #[test]
    fn parity_table_matches_python_verdicts() {
        let cases = parity_cases();

        // Every evaluation case in `test_path_policy.py` has a row; the table
        // is the port's completeness census.
        assert_eq!(cases.len(), 15, "ported test_path_policy.py cases");

        for case in cases {
            assert_eq!(
                case.policy.evaluate(&case.view),
                case.verdict,
                "parity case {}",
                case.python_case,
            );
        }
    }

    #[test]
    fn touched_tokens_captures_input_intermediate_output() {
        // V2 zfo=true: USDC→WETH; V4 zfo=false: WETH→USDC. USDC is both input
        // and final output, WETH the intermediate.
        let touched = touched_tokens(&view(vec![v2_hop(true), v4_hop(false)]));

        assert_eq!(
            touched.into_iter().collect::<BTreeSet<_>>(),
            BTreeSet::from([USDC, WETH])
        );
    }

    #[test]
    fn touched_tokens_preserves_order_uniqueness() {
        assert_eq!(touched_tokens(&view(vec![v2_hop(true)])), vec![USDC, WETH]);
    }

    #[test]
    fn first_violation_follows_python_rule_order() {
        // The duplicate guard (rule 3) is violated on the same view, but the
        // token rule (rule 2) runs first and is the verdict.
        let policy = CrawlPathPolicy {
            tokens: TokenFilter {
                deny: BTreeSet::from([USDC]),
                ..TokenFilter::default()
            },
            ..CrawlPathPolicy::default()
        };

        assert_eq!(
            policy.evaluate(&view(vec![v2_hop(true), v2_hop(false)])),
            deny_tokens(USDC, DenyWhy::Denylisted),
        );
    }

    #[test]
    fn liquidity_floor_gates_only_hops_the_proxy_measures() {
        let policy = CrawlPathPolicy {
            liquidity_floor: Some(LiquidityFloor {
                min: U256::from(500u64),
                proxy: LiquidityProxy::ReserveSum,
            }),
            ..CrawlPathPolicy::default()
        };
        let mut unmeasured = v2_hop(true);
        unmeasured.liquidity = None;

        // The proxy cannot answer for the hop, so the floor does not compare it.
        assert_eq!(
            policy.evaluate(&view(vec![unmeasured])),
            PathPolicyVerdict::Admit
        );

        let mut below = v2_hop(true);
        below.liquidity = Some(U256::from(499u64));
        assert_eq!(
            policy.evaluate(&view(vec![below])),
            PathPolicyVerdict::Deny {
                refusal: PolicyRefusal::InsufficientLiquidity {
                    pool_id: V2_POOL_ID,
                    liquidity: U256::from(499u64),
                    min: U256::from(500u64),
                },
            },
        );
    }
}
