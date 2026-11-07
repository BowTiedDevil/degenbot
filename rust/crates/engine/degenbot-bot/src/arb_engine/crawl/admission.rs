//! Registration admission — the ONE implementation of the stage order.
//!
//! One candidate path, from the identity a discovery edge carried to the
//! engine's registered path, in one order:
//!
//! 1. **memo** — the ledger's stable-refusal memo, asked pre-build per hop;
//! 2. **build** — each hop through [`Bot::construct_pool`] (the route,
//!    identity, get-or-register, refusal fold), single-flighted per pool
//!    ([`BuildFlights`]), so concurrent candidates share one construction;
//! 3. **eligibility** — direction resolution, the rejected-path memo read, the
//!    policy verdict ([`CrawlPathPolicy::evaluate`] over a
//!    [`PathCompositionView`] built from registered state), then the one
//!    mechanical gate ([`RegistrationLedger::admit_path_with_policy`]);
//! 4. **verify** — the pose-independent [`VerifyLifecycle`] per pool, at most
//!    once per session (the claim table plus the durable verified-pool fact);
//! 5. **register** — the engine registry's path registration + eager solve,
//!    with the cap refusal answering the benign `Cap` outcome.
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
use degenbot_pathfinding::directions::{
    resolve_directions, DirectionError, DirectionHop, DirectionToken,
};
use degenbot_pathfinding::PoolKind;
use degenbot_pools::solve::PoolHop;
use degenbot_pools::{BuildFailure, RegisterRefusal, RegisteredPoolFamily};
use degenbot_rpc::abi::{ContractRead, IPoolIdentity};
use degenbot_substrate::state_lock::LockSite;
use degenbot_substrate::PoolLookup;
use degenbot_uniswap::dex_identity::DexVariant;

use crate::arb_engine::crawl::flights::{
    BuildFlights, FlightEnter, FlightKey, FlightOutcome, FlightTarget,
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
    BuildRefusal, HopSignature, OutcomeLabel, PathAdmission, PolicyVerdict, PreBuildIdentity,
    RefusalKind, RegistrationLedger, RegistrationOutcome, RegistrationUnitOutcome,
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

// ── the shared memo store ──────────────────────────────────────────────────

/// The admission's shared negative-memo store: the core [`RegistrationLedger`]
/// behind a lock, so candidates on concurrent seats share ONE set of pool facts.
///
/// The Python pipeline shares its ledger across seat threads under the GIL; the
/// core has no GIL, so the lock is explicit — and [`CrawlLedger::with`] keeps
/// the guard inside the call, so a guard never crosses an `await` and a
/// candidate's future stays `Send`.
#[derive(Debug, Default)]
pub struct CrawlLedger {
    ledger: parking_lot::Mutex<RegistrationLedger>,
}

impl CrawlLedger {
    /// An empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Run `f` against the ledger under its lock. `f` must not await.
    pub fn with<R>(&self, f: impl FnOnce(&mut RegistrationLedger) -> R) -> R {
        let mut ledger = self.ledger.lock();
        f(&mut ledger)
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
    /// The shared negative memos.
    ledger: Arc<CrawlLedger>,
    /// The in-flight build table.
    flights: Arc<BuildFlights>,
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
        ledger: Arc<CrawlLedger>,
        flights: Arc<BuildFlights>,
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

    /// The shared negative memos.
    #[must_use]
    pub fn ledger(&self) -> &Arc<CrawlLedger> {
        &self.ledger
    }

    /// The in-flight build table.
    #[must_use]
    pub fn flights(&self) -> &Arc<BuildFlights> {
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
        let mut run = UnitRun::new();
        if let Some(receipt) = self.memo_stage(seq, hops, &mut run) {
            return Ok(receipt);
        }
        if let Some(receipt) = self.build_stage(seq, hops, &mut run, db).await? {
            return Ok(receipt);
        }
        if let Some(receipt) = self.eligibility_stage(seq, directions, &mut run)? {
            return Ok(receipt);
        }
        self.verify_stage(&run).await?;
        run.clock.lap();
        let Some(hop_sig) = run.hop_sig.clone() else {
            return Err(CrawlAbort::Invariant {
                detail: "the register stage ran without an admitted hop signature".to_owned(),
            });
        };
        Ok(self.register_stage(seq, &mut run, hop_sig))
    }

    /// The MEMO stage: a hop whose stable build refusal is already a pool fact
    /// answers the WHOLE path — no other hop of it can produce a different
    /// candidate.
    fn memo_stage(
        &self,
        seq: u64,
        hops: &[DiscoveredHop],
        run: &mut UnitRun,
    ) -> Option<CrawlReceipt> {
        for hop in hops {
            let key = hop.pre_build_identity();
            let memoized = self
                .ledger
                .with(|ledger| ledger.unregistrable_record(key.as_ref()).cloned());
            if let Some(record) = memoized {
                return Some(run.clock.answer(
                    seq,
                    skip(record.outcome, record.counts_as_skip, None),
                    None,
                    None,
                ));
            }
        }
        run.clock.lap();
        None
    }

    /// The BUILD stage: each hop through the one construction door,
    /// single-flighted per pool, with a stable refusal memoized as a pool fact.
    async fn build_stage(
        &self,
        seq: u64,
        hops: &[DiscoveredHop],
        run: &mut UnitRun,
        db: Option<&dyn TickMapDb>,
    ) -> Result<Option<CrawlReceipt>, CrawlAbort> {
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
                return Ok(Some(run.clock.answer(
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
                return Ok(Some(run.clock.answer(
                    seq,
                    skip(RegistrationOutcome::V4NoHash, true, None),
                    None,
                    None,
                )));
            }
            match self.build_hop(hop, block, db).await {
                Ok(pool_id) => run.pool_ids.push(pool_id),
                Err(refusal) => match classify_construction_refusal(&refusal, *hop) {
                    HopRefusal::Abort(kind) => return Err(CrawlAbort::Construction(kind)),
                    HopRefusal::Skip(refusal) => {
                        if refusal.stable {
                            // A stable refusal is a POOL fact: memoize the hop's
                            // identity so a later candidate through it spends
                            // nothing. A transient failure is never memoized.
                            let key = hop.pre_build_identity();
                            self.ledger.with(|ledger| {
                                ledger.memoize_unregistrable(
                                    key,
                                    refusal.outcome.clone(),
                                    refusal.counts_as_skip,
                                );
                            });
                        }
                        return Ok(Some(run.clock.answer(
                            seq,
                            skip(refusal.outcome, refusal.counts_as_skip, refusal.detail),
                            None,
                            None,
                        )));
                    }
                },
            }
        }
        run.v4_hops = hops
            .iter()
            .filter(|hop| matches!(hop, DiscoveredHop::V4 { .. }))
            .count();
        run.clock.lap();
        Ok(None)
    }

    /// The ELIGIBILITY stage: direction resolution, the rejected-path memo read,
    /// the policy verdict, then the one mechanical gate.
    fn eligibility_stage(
        &self,
        seq: u64,
        directions: Option<&[bool]>,
        run: &mut UnitRun,
    ) -> Result<Option<CrawlReceipt>, CrawlAbort> {
        let Some(facts) = self.registered_hops(&run.pool_ids) else {
            return Ok(Some(run.clock.answer(
                seq,
                reject(Some(
                    "a hop the crawl just built is not registered".to_owned(),
                )),
                None,
                None,
            )));
        };
        run.facts = facts;

        let resolved = self.resolve_hop_directions(directions, run)?;
        let effective = directions.or(resolved.as_deref());
        let candidate_sig: Option<HopSignature> = effective
            .filter(|directions| directions.len() == run.pool_ids.len())
            .map(|directions| {
                run.pool_ids
                    .iter()
                    .copied()
                    .zip(directions.iter().copied())
                    .collect()
            });

        if let Some(sig) = &candidate_sig {
            // A deterministic deny already recorded for this signature:
            // answering from the memo keeps the policy from re-running.
            if self.ledger.with(|ledger| ledger.path_rejected(sig)) {
                return Ok(Some(run.clock.answer(
                    seq,
                    reject(None),
                    Some(sig.clone()),
                    None,
                )));
            }
        }

        let (verdict, policy_detail) = match &candidate_sig {
            Some(sig) => match self.policy_verdict(seq, sig)? {
                Some(answer) => answer,
                None => {
                    return Ok(Some(run.clock.answer(
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
            ledger.admit_path_with_policy(&run.pool_ids, directions, resolved.as_deref(), verdict)
        });
        match admission {
            PathAdmission::DirectionMismatch => Ok(Some(run.clock.answer(
                seq,
                skip(RegistrationOutcome::DirectionMismatch, true, None),
                None,
                None,
            ))),
            PathAdmission::PolicyDenied => Ok(Some(run.clock.answer(
                seq,
                reject(policy_detail),
                candidate_sig,
                None,
            ))),
            // Another seat memoized the deny between the read above and this
            // ask: the same bounded outcome, without a detail.
            PathAdmission::RejectedPathMemo => Ok(Some(run.clock.answer(
                seq,
                reject(None),
                candidate_sig,
                None,
            ))),
            // The dup fast-path: a signature already registered answers with the
            // engine dedup's shape (`created = false`) without touching verify.
            PathAdmission::AlreadyRegistered => Ok(Some(run.clock.answer(
                seq,
                RegistrationUnitOutcome::Registered {
                    created: false,
                    v4_hops: run.v4_hops,
                },
                candidate_sig,
                None,
            ))),
            PathAdmission::Admitted { hop_sig } => {
                run.hop_sig = Some(hop_sig);
                run.clock.lap();
                Ok(None)
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
        run: &UnitRun,
    ) -> Result<Option<Vec<bool>>, CrawlAbort> {
        match directions {
            Some(pin) if pin.len() == run.pool_ids.len() => Ok(Some(pin.to_vec())),
            Some(_) => Ok(None),
            None => {
                let spellings: Vec<(String, String, String)> = run
                    .facts
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
                let hops: Vec<DirectionHop<'_>> = run
                    .facts
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

    /// The verify stage: one lifecycle per pool, at most once per session.
    async fn verify_stage(&self, run: &UnitRun) -> Result<(), CrawlAbort> {
        for facts in &run.facts {
            self.verify_hop(facts).await?;
        }
        Ok(())
    }

    /// The REGISTER stage: the engine registry's path registration + eager
    /// solve, with the cap refusal answering the benign `Cap` outcome.
    fn register_stage(&self, seq: u64, run: &mut UnitRun, hop_sig: HopSignature) -> CrawlReceipt {
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
                self.ledger
                    .with(|ledger| ledger.memoize_registered_path(hop_sig.clone()));
                run.clock.answer(
                    seq,
                    RegistrationUnitOutcome::Registered {
                        created,
                        v4_hops: run.v4_hops,
                    },
                    Some(hop_sig),
                    Some(path_id),
                )
            }
            // The benign registered-path cap stop. The latch the loop stops on
            // is the controller's (C4); the admission only names the outcome.
            Err(PathRegistrationError::RegistryFull { .. }) => {
                run.clock
                    .answer(seq, RegistrationUnitOutcome::Cap, Some(hop_sig), None)
            }
            // A transient register failure is deliberately NEVER negatively
            // memoized: a raced build or a blip must stay retryable.
            Err(PathRegistrationError::Invalid(detail)) => run.clock.answer(
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
                ReservePairArm::V2 => PoolConstruction::V2 { address: *address },
                ReservePairArm::Aerodrome => PoolConstruction::AerodromeV2 { address: *address },
            },
            DiscoveredHop::V3 { address } => PoolConstruction::V3 {
                address: *address,
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

/// One candidate's working set, threaded through the stages in execution order.
struct UnitRun {
    /// The stage clock: one mark, lapped at each stage boundary.
    clock: StageClock,
    /// The built hop pool ids, in hop order.
    pool_ids: Vec<u64>,
    /// The registered state-side facts of every built hop (the eligibility and
    /// verify stages' shared read).
    facts: Vec<RegisteredHopFacts>,
    /// The hop signature the gate admitted — what the register stage submits.
    hop_sig: Option<HopSignature>,
    /// V4 hops that reached the registration stage: the `v4_pool_count` witness,
    /// folded on `Registered` outcomes only.
    v4_hops: usize,
}

impl UnitRun {
    fn new() -> Self {
        Self {
            clock: StageClock::new(),
            pool_ids: Vec::new(),
            facts: Vec::new(),
            hop_sig: None,
            v4_hops: 0,
        }
    }
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
