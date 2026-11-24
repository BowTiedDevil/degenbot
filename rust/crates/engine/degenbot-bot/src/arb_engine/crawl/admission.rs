//! Registration admission — the ONE implementation of the stage order.
//!
//! One candidate path, from the identity a discovery edge carried to the
//! engine's registered path, in one order:
//!
//! 1. **memo** — the pre-construction facts: the ledger's stable-refusal memo
//!    per hop, then the ADR-107 D4 family capability of each discovered hop
//!    (a solve-only family answers here, before any construction);
//! 2. **build** — each hop through [`Bot::construct_pool`] (the route,
//!    identity, get-or-register, refusal fold), single-flighted per pool
//!    ([`CrawlFlights`]), so concurrent candidates share one construction;
//! 3. **eligibility** — direction resolution, the rejected-path memo read, the
//!    policy verdict ([`CrawlPathPolicy::evaluate`] over a
//!    [`PathCompositionView`] built from registered state), then the one
//!    mechanical gate ([`RegistrationLedger::admit_path_with_policy`]);
//! 4. **verify** — the pose-independent [`VerifyLifecycle`] per pool, at most
//!    once per session (the claim table plus the durable verified-pool fact);
//! 5. **register** — the engine registry's path registration + eager solve,
//!    with the cap refusal answering the benign `Cap` outcome.
//!
//! # The stage order is a typestate
//!
//! Each stage either answers the candidate with its one [`CrawlReceipt`] or
//! hands the next state on: `UnitRun` (nothing known but the clock) →
//! `Capable` (every discovered family composes) → `Built` (every hop
//! constructed and registered) → `Eligible` (the gate admitted the candidate's
//! hop signature). The eligibility gate is the only producer of
//! `Eligible` and the register stage takes nothing else, so the order cannot be
//! reordered or half-run by a later edit, and `Eligible` holds the admitted
//! signature as a value: a run that reaches registration without one has no
//! spelling.
//!
//! # One implementation, two adapters
//!
//! The *crawl* ([`crate::arb_engine::crawl::unit`]) feeds this from discovered
//! edges as its loop pulls candidates; a *direct* caller (the pure-Rust
//! settlement bot, a Rust consumer holding a candidate path) calls
//! [`RegistrationAdmission::admit`] itself. Neither adapter owns the order and
//! neither can drift it: for the same candidate sequence both answer the same
//! [`CrawlReceipt`], and folding those receipts through
//! [`PipelineReport::absorb`] answers the same report.
//!
//! Admission is pose-independent: it names no session, no pump and no driver, so
//! it runs in a process that never subscribes.
//!
//! The answer is exactly one [`CrawlReceipt`] whose stage split is
//! front-truncated at the stage that answered it, or one [`CrawlAbort`] for the
//! loud contract (a family-level construction refusal, a direction invariant
//! violation, a verification mismatch/RPC failure past the retry policy) —
//! those never become a counted skip.
//!
//! # Deliberate divergence: the identity divergence
//!
//! The Python build stage's `except Exception` net classifies a CREATE2
//! identity divergence as a *transient skip* (it only ever sees the builder's
//! `ValueError` rendering). [`ConstructionRefusal::IdentityMismatch`] is a
//! family-level refusal (ADR-055 D4) and this implementation aborts on it
//! instead of re-offering the pool forever. That is the one row the parity
//! harness records as a divergence rather than as parity.

use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use alloy::primitives::Address;
use degenbot_core::op_warn;
use degenbot_db::snapshot::TickMapDb;
use degenbot_execution::PathInfo;
use degenbot_executor::compose_capability::{self, ComposeCapabilityRefusal};
use degenbot_pathfinding::directions::{
    resolve_directions, DirectionError, DirectionHop, DirectionToken,
};
use degenbot_pathfinding::PoolKind;
use degenbot_pools::solve::PoolHop;
use degenbot_pools::solve::{HopType, MixedPoolRef};
use degenbot_pools::Arm;
use degenbot_pools::{BuildFailure, RegisterRefusal, RegisteredPoolFamily};
use degenbot_rpc::abi::{ContractRead, IPoolIdentity};
use degenbot_substrate::path_info::build_path_info;
use degenbot_substrate::state_lock::LockSite;
use degenbot_substrate::PoolLookup;
use degenbot_uniswap::dex_identity::DexVariant;

use crate::arb_engine::crawl::flights::{
    CrawlFlights, FlightEnter, FlightKey, FlightOutcome, FlightTarget,
};
use crate::arb_engine::crawl::policy::{
    oracle_verdict, registered_hop_facts, BatchPolicyOracle, CrawlPathPolicy, PathCompositionView,
    PathPolicyVerdict, PolicyCandidate, RegisteredHopFacts,
};
use crate::arb_engine::crawl::receipt::{CrawlReceipt, StageSplit};
use crate::arb_engine::lifecycle::PathRegistrationError;
use crate::arb_engine::EngineStages;
use crate::bot_core::construction_io::ConstructionIo;
use crate::bot_core::registration_ledger::{
    BuildRefusal, HopSignature, OutcomeLabel, PathAdmission, PathStanding, PolicyVerdict,
    PoolStanding, PreBuildIdentity, RefusalKind, RegistrationLedger, RegistrationLedgerHandle,
    RegistrationOutcome, RegistrationUnitOutcome, UnregistrableRecord,
};
use crate::bot_core::VerifyLifecycle;
use crate::bot_core::{
    Bot, ConstructionRefusal, ConstructionRoute, ConstructionVerdict, PoolConstruction,
    V4PoolBuildOverrides,
};

// ── the admission's input ──────────────────────────────────────────────────

/// One discovered hop: the identity the discovery edge carried, PRE-build.
///
/// The Python `PathStep` is the same triple `(address, PoolKind, hash)` as a
/// dataclass; the variants here make the invalid combinations unrepresentable —
/// a V2 hop has an address, a V4 hop has a manager and (maybe) a pool id, and
/// there is no fourth family to fall through to. The edge's `PoolKind` is the
/// family decision (R1): admission trusts it rather than re-probing the chain,
/// and a fixture test pins that the trust is equivalence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscoveredHop {
    /// A V2-table hop (Uniswap V2 and its forks); the pool's DEX preset decides
    /// which registered family the build lands in.
    V2 {
        /// The pool contract address.
        address: Address,
    },
    /// A V3 concentrated-liquidity hop.
    V3 {
        /// The pool contract address.
        address: Address,
    },
    /// A V4 hop: one `PoolManager` hosts many pools, so the pool id is the
    /// identity.
    V4 {
        /// The `PoolManager` the edge's address names.
        pool_manager: Address,
        /// The on-chain pool id — the edge's `hash`, absent on a malformed edge.
        pool_hash: Option<[u8; 32]>,
    },
}

/// The direction resolver's conceptual spelling of one hop token. The
/// protocol sentinels resolve to [`DirectionToken::Native`] only in their own
/// protocol's context — the V4 `address(0)` currency under the V4 family, the
/// Curve all-Es coin under the Curve family (ADR-097 D3). A raw sentinel
/// under any other family is not the native token.
fn hop_direction_token<'a>(
    family: RegisteredPoolFamily,
    address: &Address,
    spelling: &'a str,
) -> DirectionToken<'a> {
    let native_context = match family {
        RegisteredPoolFamily::V4 => address.is_zero(),
        RegisteredPoolFamily::Curve => *address == degenbot_pools::NATIVE_SENTINEL_ALL_ES,
        _ => false,
    };
    if native_context {
        DirectionToken::Native
    } else {
        DirectionToken::Erc20(spelling)
    }
}

impl DiscoveredHop {
    /// The discovery edge's family discriminant.
    #[must_use]
    pub const fn kind(self) -> PoolKind {
        match self {
            Self::V2 { .. } => PoolKind::V2,
            Self::V3 { .. } => PoolKind::V3,
            Self::V4 { .. } => PoolKind::V4,
        }
    }

    /// The solver hop type the edge's DB-table family names, known PRE-build.
    ///
    /// `PoolKind::V2` names the DB table, never the registered family: an
    /// Aerodrome or Camelot pair lives in that one table and lands in the
    /// `SolidlyStable` hop, so the pre-build family fact is the TABLE's family
    /// (all compose-capable) and the constructed family is assessed again after
    /// the build (`RegistrationAdmission::assess_built_candidate`).
    #[must_use]
    pub const fn hop_type(self) -> HopType {
        match self {
            Self::V2 { .. } => HopType::V2,
            Self::V3 { .. } => HopType::V3,
            Self::V4 { .. } => HopType::V4,
        }
    }

    /// The identity a build flight and the negative memos key on — known before
    /// any build (the typed spelling of the Python adapter's
    /// `RegistrationLedger::pool_memo_key`).
    #[must_use]
    pub const fn pre_build_identity(self) -> Option<PreBuildIdentity> {
        match self {
            Self::V2 { address } | Self::V3 { address } => Some(PreBuildIdentity::Address(address)),
            Self::V4 { pool_hash, .. } => match pool_hash {
                Some(pool_id) => Some(PreBuildIdentity::V4(pool_id)),
                None => None,
            },
        }
    }

    /// The flight identity: the family, the chain, and the registered identity.
    #[must_use]
    pub const fn flight_target(self) -> FlightTarget {
        match self {
            Self::V2 { address } | Self::V3 { address } => FlightTarget::Address(address),
            Self::V4 {
                pool_manager,
                pool_hash,
            } => FlightTarget::V4 {
                pool_manager,
                // A hashless V4 hop never reaches a flight: it skips first.
                pool_id: match pool_hash {
                    Some(pool_id) => pool_id,
                    None => [0; 32],
                },
            },
        }
    }
}

/// One discovered hop's PRE-BUILD facts: the identity the pool memos key on and
/// the solver hop type the edge's family names.
///
/// Both are known from the discovery edge, before any construction — which is
/// what makes the ADR-107 D4 family refusal the earliest stage that has the
/// facts to answer it. It is also the seam a FUTURE graph family is proven
/// through: today's graph yields `PoolKind::{V2,V3,V4}` only (ADR-107 C7), so
/// no discovered family can be refused in production, and the refusal is
/// driven through this value instead of a fabricated edge or a second
/// capability registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreBuildHop {
    /// The identity a pool memo keys on (`None` when the edge carries none —
    /// a hashless V4 hop).
    pub identity: Option<PreBuildIdentity>,
    /// The family the edge names.
    pub hop_type: HopType,
}

impl PreBuildHop {
    /// The facts one discovered edge carries.
    #[must_use]
    pub const fn of(hop: DiscoveredHop) -> Self {
        Self {
            identity: hop.pre_build_identity(),
            hop_type: hop.hop_type(),
        }
    }
}

// ── the admission-wide values a candidate reads ────────────────────────────

/// The admission-wide policy values: everything the shell resolves ONCE at
/// configure time and every candidate then applies.
///
/// The verify retry policy is NOT here: it is a construction value of the
/// [`VerifyLifecycle`] the admission composes, so one home owns it.
#[derive(Clone, Debug)]
pub struct AdmissionSettings {
    /// The resolved construction route (factory rungs + the generic builder).
    pub route: ConstructionRoute,
    /// The path-composition policy as data.
    pub policy: CrawlPathPolicy,
    /// A foreign policy consulted in place of `policy` when the shell installed
    /// one (the bulk-verdict escape: a policy the core cannot evaluate as data).
    pub oracle: Option<Arc<dyn BatchPolicyOracle>>,
    /// The cycle's profit token: the direction resolver's start and end, and the
    /// token the V4 native-currency sentinel is treated as equivalent to.
    pub profit_token: Address,
    /// The verify lifecycle's snapshot block (`None` = the lifecycle's own).
    pub snapshot_block: Option<u64>,
    /// An explicit construction block (`None` = the chain head at build time).
    pub state_block: Option<u64>,
}

// ── the loud contract ──────────────────────────────────────────────────────

/// Why admission aborted the caller's loop.
///
/// These are the loud-abort arms — a family-level construction refusal, a
/// direction invariant violation, a verification mismatch or an exhausted RPC
/// retry — and none of them is a counted skip: the crawl stops.
#[derive(Debug)]
pub enum CrawlAbort {
    /// A family-level construction refusal (ADR-055 D4): no route rung serves
    /// the pool, no identity selector answered, or the identity diverged.
    Construction(RefusalKind),
    /// Direction resolution refused the cycle: a hop carries neither tracked
    /// token, or the chain does not close.
    Direction(Box<DirectionError>),
    /// A pool's verify lifecycle failed (a fatal mismatch, or an RPC failure
    /// past the retry policy).
    Verify(RefusalKind),
    /// A state fact the pipeline just produced is not readable — a hop the crawl
    /// built is not registered, or a registered family the crawl cannot build.
    Invariant {
        /// What was expected and what the state answered.
        detail: String,
    },
    /// The installed foreign policy oracle failed (a shell exception, or a
    /// verdict count that does not answer the batch). Loud by design: an
    /// unattributable verdict is never a skip.
    Policy {
        /// The oracle's failure, rendered.
        detail: String,
    },
}

impl fmt::Display for CrawlAbort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Construction(kind) => write!(f, "construction refused: {kind}"),
            Self::Direction(err) => write!(f, "direction resolution failed: {err}"),
            Self::Verify(kind) => write!(f, "verify lifecycle failed: {kind}"),
            Self::Invariant { detail } => write!(f, "crawl invariant violated: {detail}"),
            Self::Policy { detail } => write!(f, "the policy oracle failed: {detail}"),
        }
    }
}

impl std::error::Error for CrawlAbort {}

// ── the admission ──────────────────────────────────────────────────────────

/// The one implementation of memoization, construction, eligibility,
/// verification, and registration.
///
/// It composes what admission needs and nothing else: the shared `Bot`
/// (construction state), the construction-I/O handle, the engine's stage seam
/// (the path registry — the registration sink), the verify home, the outcome
/// ledger, the build flights, and the policy values. It holds no session object,
/// so a process that never pumps admits candidates.
pub struct RegistrationAdmission {
    /// The shared per-chain orchestrator (construction state).
    bot: Arc<Bot>,
    /// The construction-I/O handle.
    io: Arc<ConstructionIo>,
    /// The engine's ONE stage seam: the registration sink.
    stages: Arc<EngineStages>,
    /// The pose-independent verify lifecycle (claim window + retry + fact).
    verify: Arc<VerifyLifecycle>,
    /// The shared registration standings (verify facts, refusals, path dedup).
    ledger: RegistrationLedgerHandle,
    /// The in-flight build table.
    flights: Arc<CrawlFlights>,
    /// The admission-wide policy values.
    settings: Arc<AdmissionSettings>,
}

impl RegistrationAdmission {
    /// Compose an admission over the capability handles its adapters hold.
    #[must_use]
    pub fn new(
        bot: Arc<Bot>,
        io: Arc<ConstructionIo>,
        stages: Arc<EngineStages>,
        verify: Arc<VerifyLifecycle>,
        ledger: RegistrationLedgerHandle,
        flights: Arc<CrawlFlights>,
        settings: Arc<AdmissionSettings>,
    ) -> Self {
        Self {
            bot,
            io,
            stages,
            verify,
            ledger,
            flights,
            settings,
        }
    }

    /// The engine's stage seam — the registration sink the admission writes to.
    #[must_use]
    pub fn stages(&self) -> &Arc<EngineStages> {
        &self.stages
    }

    /// The verify home the admission enters per pool.
    #[must_use]
    pub fn verify(&self) -> &Arc<VerifyLifecycle> {
        &self.verify
    }

    /// The shared registration standings.
    #[must_use]
    pub fn ledger(&self) -> &RegistrationLedgerHandle {
        &self.ledger
    }

    /// The in-flight build table.
    #[must_use]
    pub fn flights(&self) -> &Arc<CrawlFlights> {
        &self.flights
    }

    /// The admission-wide policy values.
    #[must_use]
    pub fn settings(&self) -> &Arc<AdmissionSettings> {
        &self.settings
    }

    /// Admit one candidate path.
    ///
    /// `seq` is the caller's submission order (the receipt's spine); `directions`
    /// is an operator pin (`Some`) or `None` to let the resolver answer from the
    /// registered facts; `db` is the per-use tick-map handle a head construction
    /// seeds its map from.
    ///
    /// # Errors
    ///
    /// [`CrawlAbort`] — the loud contract (see its variants).
    pub async fn admit(
        &self,
        seq: u64,
        hops: &[DiscoveredHop],
        directions: Option<&[bool]>,
        db: Option<&dyn TickMapDb>,
    ) -> Result<CrawlReceipt, CrawlAbort> {
        // The candidate's progress IS its type: each stage answers the unit or
        // hands the next state on, so the register stage cannot be reached with
        // a run the eligibility gate did not admit, and no stage can be reached
        // before the facts it reads exist.
        let pre_build: Vec<PreBuildHop> = hops.iter().copied().map(PreBuildHop::of).collect();
        let capable = match self.capability_stage(seq, &pre_build, UnitRun::new()) {
            StageAnswer::Answered(receipt) => return Ok(receipt),
            StageAnswer::Continue(capable) => capable,
        };
        let built = match self.build_stage(seq, hops, capable, db).await? {
            StageAnswer::Answered(receipt) => return Ok(receipt),
            StageAnswer::Continue(built) => built,
        };
        let eligible = match self.eligibility_stage(seq, directions, built)? {
            StageAnswer::Answered(receipt) => return Ok(receipt),
            StageAnswer::Continue(eligible) => eligible,
        };
        let eligible = self.verify_stage(eligible).await?;
        Ok(self.register_stage(seq, eligible))
    }

    /// The PRE-CONSTRUCTION stage: the ledger's stable pool refusals, then the
    /// ADR-107 D4 family capability of each discovered hop.
    ///
    /// Both facts are known before any build, so a hop whose family declares no
    /// settlement composition is refused before construction, Verify lifecycle
    /// work, registration, or solving. A pool fact answers the WHOLE path — no
    /// other hop of it can produce a different candidate.
    fn capability_stage(
        &self,
        seq: u64,
        hops: &[PreBuildHop],
        mut run: UnitRun,
    ) -> StageAnswer<Capable> {
        if let Some(receipt) = self.pre_build_answer(seq, hops, &mut run.clock) {
            return StageAnswer::Answered(receipt);
        }
        StageAnswer::Continue(Capable { clock: run.clock })
    }

    /// The pre-construction answer for one candidate, or `None` when every
    /// pre-build fact admits it (having lapped the stage into `clock`).
    ///
    /// The family evaluation is `assess_families` — the SAME static evaluator
    /// the engine's registration door consumes (ADR-107 D3), never a second
    /// registry. Today's graph cannot produce a family it refuses (ADR-107 C7),
    /// so this entry is the rule a future graph family meets; `pre_build_refusal`
    /// is the seam its refusal is proven through.
    fn pre_build_answer(
        &self,
        seq: u64,
        hops: &[PreBuildHop],
        clock: &mut StageClock,
    ) -> Option<CrawlReceipt> {
        for hop in hops {
            let memoized = self
                .ledger
                .with(|ledger| ledger.unregistrable_record(hop.identity.as_ref()).cloned());
            if let Some(record) = memoized {
                return Some(clock.answer(
                    seq,
                    skip(record.outcome, record.counts_as_skip, None),
                    None,
                    None,
                ));
            }
        }
        let hop_types: Vec<HopType> = hops.iter().map(|hop| hop.hop_type).collect();
        if let Err(refusal) = compose_capability::assess_families(Arm::Settlement, &hop_types) {
            let identity = refusal
                .hop()
                .and_then(|index| hops.get(index))
                .and_then(|hop| hop.identity);
            self.memoize_capability_refusal(&refusal, identity);
            return Some(clock.answer(seq, capability_skip(&refusal), None, None));
        }
        clock.lap();
        None
    }

    /// The pre-build family answer for one candidate taxonomy — the production
    /// stage above, driven with a hop taxonomy the current DB graph cannot
    /// yield (ADR-107 C7). `None` when the candidate may be built.
    #[cfg(test)]
    pub(crate) fn pre_build_refusal(&self, seq: u64, hops: &[PreBuildHop]) -> Option<CrawlReceipt> {
        self.pre_build_answer(seq, hops, &mut StageClock::new())
    }

    /// Settle the hop a capability refusal names as unregistrable — the pool
    /// fact ADR-107 D4 memoizes for a family refusal, keyed by the identity the
    /// proving stage holds.
    ///
    /// A pool already settled `Verified` is SUPERSEDED, not skipped:
    /// verification and composition answer different questions, and a static
    /// composition fact is evidence that no candidate path through this pool can
    /// ever register, while a verify lifecycle says nothing about composition.
    /// Leaving `Verified` in place would re-evaluate the same refusal on every
    /// later candidate; the displaced standing is reported where it happens.
    fn memoize_capability_refusal(
        &self,
        refusal: &ComposeCapabilityRefusal,
        identity: Option<PreBuildIdentity>,
    ) {
        let Some(identity) = identity else {
            return;
        };
        let record = UnregistrableRecord {
            outcome: RegistrationOutcome::UnsupportedCapability,
            counts_as_skip: true,
        };
        let displaced = self.ledger.with(|ledger| {
            match ledger.memoize_unregistrable(
                identity,
                record.outcome.clone(),
                record.counts_as_skip,
            ) {
                Ok(_) => None,
                Err(_conflict) => {
                    ledger.supersede_pool(identity, PoolStanding::Unregistrable(record))
                }
            }
        });
        if let Some(displaced) = displaced {
            op_warn!(
                domain = state,
                pool = %identity,
                displaced = ?displaced,
                refusal = %refusal,
                "a permanent capability refusal supersedes the pool's settled standing"
            );
        }
    }

    /// The BUILD stage: each hop through the one construction door,
    /// single-flighted per pool, with a stable refusal memoized as a pool fact.
    async fn build_stage(
        &self,
        seq: u64,
        hops: &[DiscoveredHop],
        capable: Capable,
        db: Option<&dyn TickMapDb>,
    ) -> Result<StageAnswer<Built>, CrawlAbort> {
        let mut clock = capable.clock;
        let head = match self.io.get_block_number().await {
            Ok(head) => head,
            // The head read is build I/O: a failure classifies as a transient
            // build refusal, exactly as the Python facade's head read does
            // inside its per-hop try.
            Err(err) => {
                let refusal = RegistrationLedger::classify_build_refusal(
                    &BuildFailure::Rpc(format!("chain head read: {err}")),
                    PoolKind::V2,
                    Some(format!("chain head read: {err}")),
                );
                return Ok(StageAnswer::Answered(clock.answer(
                    seq,
                    skip(refusal.outcome, refusal.counts_as_skip, refusal.detail),
                    None,
                    None,
                )));
            }
        };
        let block = self.settings.state_block.unwrap_or(head);
        // The DB liquidity map is a CURRENT-state cache, so it can only seed a
        // head construction: a pinned `state_block` behind the head is
        // historical, and its head-stamped rows would fail the chain-truth
        // intake gate.
        let db = if block >= head { db } else { None };

        let mut pool_ids = Vec::new();

        for hop in hops {
            // A V4 hop with no on-chain pool id cannot be built at all — a
            // bounded skip, and one no memo can hold (the edge carries no
            // identity).
            if matches!(
                hop,
                DiscoveredHop::V4 {
                    pool_hash: None,
                    ..
                }
            ) {
                return Ok(StageAnswer::Answered(clock.answer(
                    seq,
                    skip(RegistrationOutcome::V4NoHash, true, None),
                    None,
                    None,
                )));
            }
            match self.build_hop(hop, block, db).await {
                Ok(pool_id) => pool_ids.push(pool_id),
                Err(refusal) => match classify_construction_refusal(&refusal, *hop) {
                    HopRefusal::Abort(kind) => return Err(CrawlAbort::Construction(kind)),
                    HopRefusal::Skip(refusal) => {
                        if refusal.stable {
                            // A stable refusal is a POOL fact: memoize the hop's
                            // identity so a later candidate through it spends
                            // nothing. A transient failure is never memoized.
                            if let Some(key) = hop.pre_build_identity() {
                                let settled = self.ledger.with(|ledger| {
                                    ledger.memoize_unregistrable(
                                        key,
                                        refusal.outcome.clone(),
                                        refusal.counts_as_skip,
                                    )
                                });
                                if let Err(conflict) = settled {
                                    // A verified pool is a REGISTERED pool, so
                                    // its construction is get-or-register and
                                    // cannot refuse stably: a settled verified
                                    // standing here is state corruption, which a
                                    // counted skip must never paper over.
                                    return Err(CrawlAbort::Invariant {
                                        detail: format!(
                                            "registration standing conflict: {conflict}"
                                        ),
                                    });
                                }
                            }
                        }
                        return Ok(StageAnswer::Answered(clock.answer(
                            seq,
                            skip(refusal.outcome, refusal.counts_as_skip, refusal.detail),
                            None,
                            None,
                        )));
                    }
                },
            }
        }
        let v4_hops = hops
            .iter()
            .filter(|hop| matches!(hop, DiscoveredHop::V4 { .. }))
            .count();
        clock.lap();
        Ok(StageAnswer::Continue(Built {
            clock,
            pool_ids,
            v4_hops,
        }))
    }

    /// The ELIGIBILITY stage: direction resolution, the rejected-path memo read,
    /// the policy verdict, then the one mechanical gate. Every early answer here
    /// carries the built candidate's V4 count beside the receipt.
    fn eligibility_stage(
        &self,
        seq: u64,
        directions: Option<&[bool]>,
        built: Built,
    ) -> Result<StageAnswer<Eligible>, CrawlAbort> {
        let Built {
            mut clock,
            pool_ids,
            v4_hops,
        } = built;
        let Some(facts) = self.registered_hops(&pool_ids) else {
            return Ok(StageAnswer::Answered(clock.answer(
                seq,
                reject(Some(
                    "a hop the crawl just built is not registered".to_owned(),
                )),
                None,
                None,
            )));
        };

        let resolved = self.resolve_hop_directions(directions, &facts)?;
        let effective = directions.or(resolved.as_deref());
        let candidate_sig: Option<HopSignature> = effective
            .filter(|directions| directions.len() == pool_ids.len())
            .map(|directions| {
                pool_ids
                    .iter()
                    .copied()
                    .zip(directions.iter().copied())
                    .collect()
            });

        if let Some(sig) = &candidate_sig {
            // A deterministic deny already recorded for this signature:
            // answering from the memo keeps the static facts from re-running.
            if self.ledger.with(|ledger| ledger.path_rejected(sig)) {
                return Ok(StageAnswer::Answered(clock.answer(
                    seq,
                    reject(None),
                    Some(sig.clone()),
                    None,
                )));
            }
        }

        // ADR-107 D4: the constructed candidate's static composition facts,
        // answered here — after construction, before the Verify lifecycle, the
        // registry commit, or any eager solve.
        if let Some(sig) = &candidate_sig {
            if let Some(receipt) = self.capability_answer(seq, &mut clock, sig, &facts) {
                return Ok(StageAnswer::Answered(receipt));
            }
        }

        let (verdict, policy_detail) = match &candidate_sig {
            Some(sig) => match self.policy_verdict(seq, sig)? {
                Some(answer) => answer,
                None => {
                    return Ok(StageAnswer::Answered(clock.answer(
                        seq,
                        reject(Some(
                            "a built hop has no composition facts in state".to_owned(),
                        )),
                        Some(sig.clone()),
                        None,
                    )));
                }
            },
            None => (PolicyVerdict::Admitted, None),
        };

        let admission = self.ledger.with(|ledger| {
            ledger.admit_path_with_policy(&pool_ids, directions, resolved.as_deref(), verdict)
        });
        match admission {
            PathAdmission::DirectionMismatch => Ok(StageAnswer::Answered(clock.answer(
                seq,
                skip(RegistrationOutcome::DirectionMismatch, true, None),
                None,
                None,
            ))),
            PathAdmission::PolicyDenied => Ok(StageAnswer::Answered(clock.answer(
                seq,
                reject(policy_detail),
                candidate_sig,
                None,
            ))),
            // Another seat memoized the deny between the read above and this
            // ask: the same bounded outcome, without a detail.
            PathAdmission::RejectedPathMemo => Ok(StageAnswer::Answered(clock.answer(
                seq,
                reject(None),
                candidate_sig,
                None,
            ))),
            // The dup fast-path: a signature already registered answers with the
            // engine dedup's shape (`created = false`) without touching verify.
            PathAdmission::AlreadyRegistered => Ok(StageAnswer::Answered(clock.answer(
                seq,
                RegistrationUnitOutcome::Registered {
                    created: false,
                    v4_hops,
                },
                candidate_sig,
                None,
            ))),
            PathAdmission::Admitted { hop_sig } => {
                clock.lap();
                Ok(StageAnswer::Continue(Eligible {
                    clock,
                    hop_sig,
                    facts,
                    v4_hops,
                }))
            }
        }
    }

    /// The direction answer: the caller's pin when one was supplied, else the
    /// resolver's over the registered facts. `None` means the pin disagrees with
    /// the path's hop count — the gate's direction-mismatch arm answers, and
    /// neither the memo nor the policy is consulted with a signature that cannot
    /// exist.
    fn resolve_hop_directions(
        &self,
        directions: Option<&[bool]>,
        facts: &[RegisteredHopFacts],
    ) -> Result<Option<Vec<bool>>, CrawlAbort> {
        match directions {
            Some(pin) if pin.len() == facts.len() => Ok(Some(pin.to_vec())),
            Some(_) => Ok(None),
            None => {
                let spellings: Vec<(String, String, String)> = facts
                    .iter()
                    .map(|facts| {
                        (
                            facts.token0.to_string(),
                            facts.token1.to_string(),
                            facts.identity_label(),
                        )
                    })
                    .collect();
                // The conceptual spelling of each hop token: the protocol
                // sentinels resolve to the conceptual native token only in
                // their own protocol's context (ADR-097 D3), so raw zero
                // never crosses the resolver as a generic token spelling.
                let hops: Vec<DirectionHop<'_>> = facts
                    .iter()
                    .zip(spellings.iter())
                    .map(|(facts, (token0, token1, identity))| DirectionHop {
                        token0: hop_direction_token(facts.family, &facts.token0, token0),
                        token1: hop_direction_token(facts.family, &facts.token1, token1),
                        identity,
                    })
                    .collect();
                let profit = self.settings.profit_token.to_string();
                let input = DirectionToken::Erc20(&profit);
                match resolve_directions(&hops, &input, &profit) {
                    Ok(resolved) => Ok(Some(resolved.zero_for_one)),
                    Err(err) => Err(CrawlAbort::Direction(err)),
                }
            }
        }
    }

    /// The verify stage: one lifecycle per pool, at most once per session. The
    /// admitted run passes through with its verify lap closed.
    async fn verify_stage(&self, mut eligible: Eligible) -> Result<Eligible, CrawlAbort> {
        for facts in &eligible.facts {
            self.verify_hop(facts).await?;
        }
        eligible.clock.lap();
        Ok(eligible)
    }

    /// The REGISTER stage: the engine registry's path registration + eager
    /// solve, with the cap refusal answering the benign `Cap` outcome.
    ///
    /// Its input is the admitted `Eligible` state and nothing else, so the
    /// signature submitted here is always the one the gate admitted.
    fn register_stage(&self, seq: u64, eligible: Eligible) -> CrawlReceipt {
        let Eligible {
            mut clock,
            hop_sig,
            v4_hops,
            ..
        } = eligible;
        let hops: Vec<PoolHop> = hop_sig
            .iter()
            .map(|&(pool_id, zero_for_one)| PoolHop {
                pool_id,
                zero_for_one,
            })
            .collect();
        match self.stages.register_and_solve_path(hops) {
            Ok((path_id, created)) => {
                // Only a completed registration enters the memo: stable
                // negatives entered theirs at their own sites, and a failed
                // verify stays retriable.
                let displaced = self.ledger.with(|ledger| {
                    match ledger.memoize_registered_path(hop_sig.clone()) {
                        Ok(_) => None,
                        // The engine's registry holds the path whatever a deny
                        // settled first, so the deny is the stale fact.
                        Err(_deny) => {
                            ledger.supersede_path(hop_sig.clone(), PathStanding::Registered)
                        }
                    }
                });
                if let Some(displaced) = displaced {
                    op_warn!(
                        domain = state,
                        path = ?hop_sig,
                        displaced = ?displaced,
                        "a completed registration retracts a settled path deny: the engine holds the path, so the deny was stale"
                    );
                }
                clock.answer(
                    seq,
                    RegistrationUnitOutcome::Registered { created, v4_hops },
                    Some(hop_sig),
                    Some(path_id),
                )
            }
            // The benign registered-path cap stop. The latch the loop stops on
            // is the controller's (C4); the admission only names the outcome.
            Err(PathRegistrationError::RegistryFull { .. }) => {
                clock.answer(seq, RegistrationUnitOutcome::Cap, Some(hop_sig), None)
            }
            // A permanent compose-capability refusal (ADR-107 D1). The
            // post-build gate above already answers this before the Verify
            // lifecycle, so a refusal arriving here is the engine's own door
            // speaking last: it reports through the same bounded outcome rather
            // than as a transient register failure.
            Err(PathRegistrationError::UnsupportedCapability(refusal)) => {
                clock.answer(seq, capability_skip(&refusal), Some(hop_sig), None)
            }
            // A transient register failure is deliberately NEVER negatively
            // memoized: a raced build or a blip must stay retryable.
            Err(PathRegistrationError::Invalid(detail)) => clock.answer(
                seq,
                RegistrationUnitOutcome::RegisterFailed {
                    detail: Some(detail),
                },
                Some(hop_sig),
                None,
            ),
        }
    }

    /// Build one hop, single-flighted: a peer rides the leader's answer.
    pub(crate) async fn build_hop(
        &self,
        hop: &DiscoveredHop,
        block: u64,
        db: Option<&dyn TickMapDb>,
    ) -> Result<u64, ConstructionRefusal> {
        let key = FlightKey {
            kind: hop.kind(),
            chain_id: self.bot.chain_id(),
            target: hop.flight_target(),
        };
        match self.flights.enter(key) {
            FlightEnter::Peer(peer) => match peer.wait().await {
                FlightOutcome::Built(pool_id) => Ok(pool_id),
                FlightOutcome::Failed(refusal) => Err(refusal),
            },
            FlightEnter::Lead(mut lead) => {
                let answer = self.construct_hop(hop, block, db).await;
                lead.publish(match &answer {
                    Ok(pool_id) => FlightOutcome::Built(*pool_id),
                    Err(refusal) => FlightOutcome::Failed(refusal.clone()),
                });
                answer
            }
        }
    }

    /// Construct and register one hop through the one construction door.
    async fn construct_hop(
        &self,
        hop: &DiscoveredHop,
        block: u64,
        db: Option<&dyn TickMapDb>,
    ) -> Result<u64, ConstructionRefusal> {
        let request = match hop {
            DiscoveredHop::V2 { address } => match self.reserve_pair_arm(*address, block).await? {
                // The crawl resolves no identity, so no factory evidence
                // rides the request: the route reads it once and the build
                // consumes that read.
                ReservePairArm::V2 => PoolConstruction::V2 {
                    address: *address,
                    factory: None,
                },
                ReservePairArm::Aerodrome => PoolConstruction::AerodromeV2 {
                    address: *address,
                    factory: None,
                },
            },
            DiscoveredHop::V3 { address } => PoolConstruction::V3 {
                address: *address,
                factory: None,
                db,
                // The tick-word fetcher and the slot-layout override are driver
                // values the crawl never supplies: the door attaches the core's
                // own source and resolves the layout from the deployment table.
                fetcher: None,
                slot_layout: None,
            },
            DiscoveredHop::V4 {
                pool_manager,
                pool_hash,
            } => PoolConstruction::V4 {
                pool_manager: *pool_manager,
                pool_id: pool_hash.ok_or_else(|| {
                    ConstructionRefusal::Skipped(BuildFailure::MissingIdentity(
                        "a V4 hop carries no on-chain pool id".to_owned(),
                    ))
                })?,
                overrides: V4PoolBuildOverrides::default(),
                db,
            },
        };
        match self
            .bot
            .construct_pool(&self.settings.route, request, self.io.as_ref(), Some(block))
            .await
        {
            ConstructionVerdict::Constructed { pool, .. } => Ok(pool.pool_id),
            ConstructionVerdict::Refused { refusal, .. } => Err(refusal),
        }
    }

    /// Which construction arm a V2-table hop takes.
    ///
    /// `PoolKind::V2` names the DB table, not the registered family: an
    /// Aerodrome (Solidly) pair lives in the same table but registers under its
    /// own family. The DEX preset resolves from the pool's on-chain `factory()`
    /// — the read the V2 build performs anyway, and one the construction handle's
    /// block-scoped read memo serves a second time for free — so the family split
    /// costs no extra round trip. Only the Aerodrome preset flips the arm, which
    /// is exactly the split the Python resolver performs.
    async fn reserve_pair_arm(
        &self,
        address: Address,
        block: u64,
    ) -> Result<ReservePairArm, ConstructionRefusal> {
        let registered = {
            let state = self.bot.state_arc();
            let guard = state.read_at(LockSite::Registration);
            guard
                .registered_pool_by_address(&address)
                .map(|(_, family)| family)
        };
        let aerodrome = match registered {
            Some(RegisteredPoolFamily::V2) => false,
            Some(RegisteredPoolFamily::AerodromeV2) => true,
            _ => {
                let factory = self
                    .io
                    .read(address, IPoolIdentity::factoryCall {}, Some(block))
                    .await
                    .map_err(|err| {
                        ConstructionRefusal::Skipped(BuildFailure::Rpc(format!(
                            "reserve-pair factory read: {err}"
                        )))
                    })?;
                let preset = crate::bot_core::pool_builder::builder::resolve_v2_dex(
                    self.io.as_ref(),
                    address,
                    factory,
                    Some(block),
                )
                .await
                .map_err(|err| {
                    ConstructionRefusal::Skipped(BuildFailure::Rpc(format!(
                        "reserve-pair DEX preset read: {err}"
                    )))
                })?;
                preset.is_some_and(|preset| {
                    matches!(
                        preset.variant,
                        DexVariant::AerodromeV2Volatile | DexVariant::AerodromeV2Stable
                    )
                })
            }
        };
        Ok(if aerodrome {
            ReservePairArm::Aerodrome
        } else {
            ReservePairArm::V2
        })
    }

    /// The post-build capability answer for one direction-resolved candidate:
    /// `Some(receipt)` when a static composition fact refuses it (ADR-107 D4),
    /// with the memos the proving stage can key on settled.
    ///
    /// A hop-anchored (family) refusal is a POOL fact: no candidate path through
    /// that pool can compose, so the memo keys on the hop's identity. A
    /// whole-route refusal keys on the constructed hop signature, so a
    /// re-sighting answers from the rejected-path memo instead of re-projecting
    /// the route.
    ///
    /// A candidate shorter than two hops is MALFORMED, not uncomposable: the
    /// shape key is defined over well-formed routes, so assessing one would
    /// relabel a structural arity fault as permanent capability and
    /// negative-memoize a pool that never had a route to judge. Such a candidate
    /// passes through to registration, which owns the arity refusal
    /// ([`PathRegistrationError::Invalid`], a transient register failure that is
    /// never memoized).
    fn capability_answer(
        &self,
        seq: u64,
        clock: &mut StageClock,
        sig: &HopSignature,
        facts: &[RegisteredHopFacts],
    ) -> Option<CrawlReceipt> {
        if sig.len() < 2 {
            return None;
        }
        let hop_types: Vec<HopType> = facts.iter().map(|facts| facts.hop_type).collect();
        match self.assess_built_candidate(sig, &hop_types) {
            Some(Ok(())) => None,
            Some(Err(refusal)) => {
                if let Some(index) = refusal.hop() {
                    self.memoize_capability_refusal(
                        &refusal,
                        facts
                            .get(index)
                            .and_then(RegisteredHopFacts::pre_build_identity),
                    );
                } else {
                    let settled = self
                        .ledger
                        .with(|ledger| ledger.memoize_rejected_path(sig.clone()));
                    if let Err(conflict) = settled {
                        op_warn!(
                            domain = state,
                            path = ?sig,
                            conflict = %conflict,
                            "a route capability refusal contradicts a completed registration"
                        );
                    }
                }
                op_warn!(
                    domain = state,
                    route = %refusal,
                    hop = ?refusal.hop(),
                    family = ?refusal.family(),
                    families = ?refusal.families(),
                    "a constructed candidate is permanently outside settlement command composition"
                );
                Some(clock.answer(seq, capability_skip(&refusal), Some(sig.clone()), None))
            }
            // The route projection is unreadable: the family gate proved a
            // command arm for every hop, so this is a state fact the pipeline
            // just produced — the same rejection `registered_hops` answers.
            None => Some(clock.answer(
                seq,
                reject(Some(
                    "a built route has no command-stream projection".to_owned(),
                )),
                Some(sig.clone()),
                None,
            )),
        }
    }

    /// The built candidate's static composition verdict (ADR-107 D3's second
    /// fact level): the families the pools actually registered under, then the
    /// whole-route `PathInfo` the composer reads.
    ///
    /// `None` means the route has no command-stream projection — a state fact
    /// the caller answers as a rejection, because the family evaluation above
    /// already proved a command arm for every hop.
    fn assess_built_candidate(
        &self,
        hop_sig: &[(u64, bool)],
        hop_types: &[HopType],
    ) -> Option<Result<(), ComposeCapabilityRefusal>> {
        if let Err(refusal) = compose_capability::assess_families(Arm::Settlement, hop_types) {
            return Some(Err(refusal));
        }
        let path_info = self.route_path_info(hop_sig, hop_types)?;
        Some(compose_capability::assess_route(hop_types, &path_info))
    }

    /// The whole-route `PathInfo` for one direction-resolved candidate — the
    /// SAME projection the composer, the apply-time relevance derivation, and
    /// the inline-sim hook read (ADR-107 D3), built from the directions the
    /// candidate actually resolved.
    fn route_path_info(&self, hop_sig: &[(u64, bool)], hop_types: &[HopType]) -> Option<PathInfo> {
        let pools: Vec<MixedPoolRef> = hop_sig
            .iter()
            .zip(hop_types.iter())
            .map(|(&(pool_key, zero_for_one), &hop_type)| MixedPoolRef {
                hop_type,
                pool_key,
                zero_for_one,
            })
            .collect();
        let state = self.bot.state_arc();
        let guard = state.read_at(LockSite::Registration);
        build_path_info(&guard, &pools).ok()
    }

    /// The registered state-side facts for every built hop.
    fn registered_hops(&self, pool_ids: &[u64]) -> Option<Vec<RegisteredHopFacts>> {
        let state = self.bot.state_arc();
        let guard = state.read_at(LockSite::Registration);
        pool_ids
            .iter()
            .map(|&pool_id| registered_hop_facts(&guard, pool_id))
            .collect()
    }

    /// The composition view for one candidate signature.
    fn composition_view(&self, hop_sig: &[(u64, bool)]) -> Option<PathCompositionView> {
        let state = self.bot.state_arc();
        let guard = state.read_at(LockSite::Registration);
        PathCompositionView::from_registered(
            &guard,
            hop_sig,
            self.settings.policy.liquidity_floor.as_ref(),
        )
    }

    /// The policy verdict for one candidate signature, or `None` when a built hop
    /// has no composition facts in state.
    ///
    /// The configured policy is consulted as DATA
    /// ([`CrawlPathPolicy::evaluate`]) unless the shell installed a foreign
    /// oracle, which then answers in its place — one candidate per consult,
    /// keyed by the candidate's submission sequence.
    ///
    /// # Errors
    ///
    /// [`CrawlAbort::Policy`] when the installed oracle fails or answers an
    /// unattributable verdict list.
    fn policy_verdict(
        &self,
        seq: u64,
        hop_sig: &[(u64, bool)],
    ) -> Result<Option<(PolicyVerdict, Option<String>)>, CrawlAbort> {
        let Some(view) = self.composition_view(hop_sig) else {
            return Ok(None);
        };
        let verdict = match &self.settings.oracle {
            Some(oracle) => {
                let candidate = PolicyCandidate {
                    seq,
                    hop_sig: hop_sig.to_vec(),
                    view,
                };
                oracle_verdict(oracle.as_ref(), &candidate).map_err(|err| CrawlAbort::Policy {
                    detail: err.to_string(),
                })?
            }
            None => self.settings.policy.evaluate(&view),
        };
        Ok(Some(match verdict {
            PathPolicyVerdict::Admit => (PolicyVerdict::Admitted, None),
            PathPolicyVerdict::Deny { refusal } => {
                (PolicyVerdict::Denied, Some(format!("{refusal:?}")))
            }
        }))
    }

    /// One pool's verify lifecycle, at most once per session — the verify home
    /// owns the claim window, the durable fact, and the bounded retry, so two
    /// candidates (from either adapter) share one run and each receives its
    /// outcome.
    ///
    /// A DEFERRED outcome (an incomplete drain) is a successful return that
    /// leaves the pool parked with its verification still owed: this step
    /// reports no abort, and a later lifecycle call for the same pool retries it
    /// (the deferral records no durable fact).
    async fn verify_hop(&self, facts: &RegisteredHopFacts) -> Result<(), CrawlAbort> {
        let key = match facts.family {
            RegisteredPoolFamily::V3 => {
                let address = facts.address.ok_or_else(|| CrawlAbort::Invariant {
                    detail: "a V3 pool registered without an address".to_owned(),
                })?;
                PreBuildIdentity::Address(address)
            }
            RegisteredPoolFamily::V4 => {
                let (_, pool_id) = facts.v4.ok_or_else(|| CrawlAbort::Invariant {
                    detail: "a V4 pool registered without a pool id".to_owned(),
                })?;
                PreBuildIdentity::V4(pool_id)
            }
            RegisteredPoolFamily::V2 => {
                // A V2 pair runs no lifecycle; its one verify-stage duty is the
                // asymmetric-fee warning (both directions priced differently is
                // an operator-visible pool anomaly).
                if let Some((fee_token0, fee_token1)) = facts.fee_pair {
                    if fee_token0 != fee_token1 {
                        op_warn!(
                            domain = state,
                            pool = %facts.identity_label(),
                            fee_token0 = ?fee_token0,
                            fee_token1 = ?fee_token1,
                            "asymmetric V2 fees detected"
                        );
                    }
                }
                return Ok(());
            }
            // An Aerodrome pair prices both directions with ONE fee, so there is
            // no asymmetry to warn about and no lifecycle to run.
            RegisteredPoolFamily::AerodromeV2 => return Ok(()),
            // Admission builds only V2/V3/V4 hops: a multi-token registration
            // behind a crawled hop's pool id is a state-side violation.
            RegisteredPoolFamily::Curve
            | RegisteredPoolFamily::BalancerWeighted
            | RegisteredPoolFamily::BalancerStable => {
                return Err(CrawlAbort::Invariant {
                    detail: format!(
                        "the crawl built a {} pool — only V2/V3/V4 hops are crawlable",
                        facts.family.tag()
                    ),
                });
            }
        };
        let lookup = match key {
            PreBuildIdentity::Address(address) => PoolLookup::Address(address),
            PreBuildIdentity::V4(pool_id) => {
                let (pool_manager, _) = facts.v4.ok_or_else(|| CrawlAbort::Invariant {
                    detail: "a V4 pool registered without a manager".to_owned(),
                })?;
                PoolLookup::V4 {
                    pool_manager,
                    pool_id,
                }
            }
        };
        self.verify
            .run(lookup, self.settings.snapshot_block)
            .await
            .map(|_outcome| ())
            .map_err(CrawlAbort::Verify)
    }
}

/// The reserve-pair DEX split the V2 table cannot express.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReservePairArm {
    /// A constant-product pair (Uniswap V2 and the V2-style forks).
    V2,
    /// An Aerodrome (Solidly) pair, which registers under its own family.
    Aerodrome,
}

/// How one hop's construction refusal answers the candidate.
pub(crate) enum HopRefusal {
    /// A family-level refusal: the crawl stops (ADR-055 D4).
    Abort(RefusalKind),
    /// A classified skip; a STABLE refusal also memoizes the pool as a fact.
    Skip(BuildRefusal),
}

/// One candidate at the memo stage: nothing is known yet but the clock.
struct UnitRun {
    /// The stage clock: one mark, lapped at each stage boundary.
    clock: StageClock,
}

/// A candidate whose every discovered family declares settlement command
/// composition: the ONE state the build stage accepts.
///
/// The pre-build family gate is the only producer of `Capable`, so a candidate
/// whose family cannot compose has no spelling that reaches construction.
struct Capable {
    /// The stage clock, carried across the stage boundary.
    clock: StageClock,
}

impl UnitRun {
    fn new() -> Self {
        Self {
            clock: StageClock::new(),
        }
    }
}

/// What one stage answered: the candidate's receipt, or the state the next stage
/// runs.
///
/// The two arms are the whole transition vocabulary — a stage terminates the
/// unit or hands the next state on, and no stage can fabricate a later state's
/// values.
#[expect(
    clippy::large_enum_variant,
    reason = "the answered arm is the receipt the stage hands back by value, and the value is consumed in the same expression; boxing it would add a heap hop per candidate to shrink an enum nothing stores"
)]
enum StageAnswer<Next> {
    /// The stage answered the candidate: this receipt is its whole result.
    Answered(CrawlReceipt),
    /// The stage passed the candidate on in its next state.
    Continue(Next),
}

/// A candidate whose every hop is built and registered as a pool.
struct Built {
    /// The stage clock, carried across the stage boundary.
    clock: StageClock,
    /// The built hop pool ids, in hop order.
    pool_ids: Vec<u64>,
    /// V4 hops the candidate carried — the `v4_pool_count` witness, folded on
    /// `Registered` outcomes (created and duplicate alike).
    v4_hops: usize,
}

/// A candidate the eligibility gate admitted: the ONE state the register stage
/// accepts.
///
/// The admitted hop signature is a value here, never an `Option`, so a run that
/// reached registration without one has no spelling: no arm fails on it.
struct Eligible {
    /// The stage clock, carried across the stage boundary.
    clock: StageClock,
    /// The hop signature the gate admitted — what the register stage submits.
    hop_sig: HopSignature,
    /// The registered state-side facts of every built hop (the verify stage's
    /// input).
    facts: Vec<RegisteredHopFacts>,
    /// V4 hops the candidate carried — the `v4_pool_count` witness.
    v4_hops: usize,
}

/// Classify one construction refusal into the candidate's answer.
///
/// Two classes: a family-level refusal aborts (ADR-055 D4), and everything else
/// classifies on the registration ledger's build-refusal taxonomy — the
/// stable-vs-transient fact that decides whether the pool becomes a memoized fact
/// or stays retryable.
pub(crate) fn classify_construction_refusal(
    refusal: &ConstructionRefusal,
    hop: DiscoveredHop,
) -> HopRefusal {
    let failure = match &refusal {
        ConstructionRefusal::UnsupportedFamily { .. }
        | ConstructionRefusal::IdentityMismatch(_) => {
            return HopRefusal::Abort(RefusalKind::from_construction_refusal(
                refusal,
                registered_family(hop.kind()),
            ));
        }
        ConstructionRefusal::Skipped(failure) => failure.clone(),
        ConstructionRefusal::Registration(reason) => match reason {
            // A V4 admission refusal keeps the `BuildFailure` taxonomy's own
            // class even when it crosses from the write door: a hooked pool is a
            // STABLE pool fact that carries its own counter, not a generic skip.
            RegisterRefusal::Admission { failure, .. } => failure.clone(),
            // A raced registration self-heals on the next offer (the door folds
            // `AlreadyRegistered` itself, so this arm is a re-read miss), and a
            // spec violation is re-offered as the same retryable failure.
            RegisterRefusal::AlreadyRegistered { .. } | RegisterRefusal::SpecViolation { .. } => {
                BuildFailure::Transient(format!("pool registration refused: {reason:?}"))
            }
        },
    };
    HopRefusal::Skip(RegistrationLedger::classify_build_refusal(
        &failure,
        hop.kind(),
        Some(format!("{failure:?}")),
    ))
}

/// The registered family a discovery kind names, for a refusal's rendering.
fn registered_family(kind: PoolKind) -> Option<RegisteredPoolFamily> {
    match kind {
        PoolKind::V2 => Some(RegisteredPoolFamily::V2),
        PoolKind::V3 => Some(RegisteredPoolFamily::V3),
        PoolKind::V4 => Some(RegisteredPoolFamily::V4),
        _ => None,
    }
}

fn skip(
    label: RegistrationOutcome,
    counts_as_skip: bool,
    detail: Option<String>,
) -> RegistrationUnitOutcome {
    RegistrationUnitOutcome::Skip {
        label: OutcomeLabel::Vocabulary(label),
        counts_as_skip,
        detail,
    }
}

fn reject(detail: Option<String>) -> RegistrationUnitOutcome {
    RegistrationUnitOutcome::Reject { detail }
}

/// The bounded capability refusal: a counted skip carrying the
/// `unsupported-capability` vocabulary member (ADR-107 D9), with the
/// structured refusal — family, hop, route reason — in the log-only detail.
fn capability_skip(refusal: &ComposeCapabilityRefusal) -> RegistrationUnitOutcome {
    skip(
        RegistrationOutcome::UnsupportedCapability,
        true,
        Some(refusal.to_string()),
    )
}

/// The candidate's stage clock: one running mark, lapped at each stage boundary.
struct StageClock {
    split: StageSplit,
    mark: Instant,
}

impl StageClock {
    fn new() -> Self {
        Self {
            split: StageSplit::new(),
            mark: Instant::now(),
        }
    }

    /// Close the stage now running into the split.
    fn lap(&mut self) {
        let now = Instant::now();
        let ns = u64::try_from(now.duration_since(self.mark).as_nanos()).unwrap_or(u64::MAX);
        let _ = self.split.lap(ns);
        self.mark = now;
    }

    /// Close the answering stage and mint the candidate's receipt.
    fn answer(
        &mut self,
        seq: u64,
        outcome: RegistrationUnitOutcome,
        hop_sig: Option<HopSignature>,
        path_id: Option<u64>,
    ) -> CrawlReceipt {
        self.lap();
        CrawlReceipt {
            seq,
            hop_sig,
            outcome,
            split: self.split,
            path_id,
        }
    }
}

#[cfg(test)]
mod tests {
    //! The stage seam's own type pins.
    //!
    //! The repo has no compile-fail harness (no `trybuild` corpus), so the
    //! typestate's compile-time claim is asserted where rustc still enforces it:
    //! a fn-pointer coercion names a stage's exact signature, and it stops
    //! compiling the moment the seam widens back to an unadmitted run or to an
    //! optional signature.

    use super::*;

    /// The eligibility stage's answer: the gate hands the admitted state on, or
    /// the receipt terminates the unit.
    type GateAnswer = Result<StageAnswer<Eligible>, CrawlAbort>;

    /// The admitted state's hop signature, read as a value: the coercion at the
    /// call site is what proves the field's type.
    fn admitted_signature(eligible: &Eligible) -> &HopSignature {
        &eligible.hop_sig
    }

    /// The register stage takes the eligibility gate's answer and nothing else,
    /// by value.
    ///
    /// The coercions are the assertion: this file stops compiling if the stage
    /// widens to a `&mut UnitRun`, a `Built`, an `Option<HopSignature>`, or a
    /// signature threaded beside the state instead of inside it.
    #[test]
    fn the_register_seam_is_the_admitted_state() {
        let register: fn(&RegistrationAdmission, u64, Eligible) -> CrawlReceipt =
            RegistrationAdmission::register_stage;
        let signature: fn(&Eligible) -> &HopSignature = admitted_signature;
        let _ = (register, signature);
    }

    /// The pre-construction gate answers with the candidate's receipt or hands
    /// the `Capable` state on — the ONE state the build stage takes. The
    /// coercion is the assertion: it stops compiling if the stage widens back to
    /// a pre-family `UnitRun`.
    #[test]
    fn the_pre_build_gate_answers_or_hands_on_a_capable_candidate() {
        type CapabilityAnswer = StageAnswer<Capable>;
        let gate: fn(&RegistrationAdmission, u64, &[PreBuildHop], UnitRun) -> CapabilityAnswer =
            RegistrationAdmission::capability_stage;
        let _ = gate;
    }

    /// The eligibility gate either answers with the candidate's receipt or hands
    /// the admitted state on: there is no third arm carrying a half-admitted run.
    #[test]
    fn the_eligibility_gate_answers_or_hands_on_the_admitted_state() {
        let gate: fn(&RegistrationAdmission, u64, Option<&[bool]>, Built) -> GateAnswer =
            RegistrationAdmission::eligibility_stage;
        let _ = gate;
    }
}
