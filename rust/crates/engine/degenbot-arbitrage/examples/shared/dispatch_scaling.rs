//! The controlled same-head dispatch-scaling benchmark: the production dispatch
//! entry point (`dispatch_profitable_results`) driven over ONE scripted chain,
//! ONE candidate list, ONE deployment declaration, and ONE provider state, with
//! the storage representation the only variable — the shipped legacy memo +
//! prefetch lane, or an already-settled BAL delta generation the session's
//! canonical-head pipeline prepared off the fan-out's path.
//!
//! # Why the corpus is scripted, and what that costs
//!
//! The live OFF/ON pair could not price the mechanism: the two windows ran at
//! disjoint heads over different candidate mixes, so no fan-out had a
//! counterpart. This bench removes that confound entirely by scripting the
//! chain and the market: every leg runs the SAME head, the SAME candidates, the
//! SAME deployment, and the SAME scripted storage chain, so a lane's difference
//! is the storage representation and nothing else.
//!
//! The price is that the corpus is synthetic in three ways, and the report
//! states all three beside every number:
//!
//! 1. **The market is one scripted chain.** The injected executor runtime is a
//!    minimal value-sensitive reader ([`read_runtime`]): it SLOADs the
//!    executor's own [`READ_SLOT`] word and returns a two-word execution receipt
//!    whose `gross_profit` is a function of that word, so every simulated
//!    candidate performs a real storage read and its OUTCOME CATEGORY moves with
//!    the stored value. The priced figures are still a receipt this bench wrote,
//!    not a swap's retained profit: nothing here measures a real opportunity.
//! 2. **Candidates differ by pool identity only.** Candidate `i` routes the
//!    same two-hop WETH -> TOKEN -> WETH shape through its own pool pair, so
//!    the path identities are distinct (which is what makes the prefetch and
//!    the generation scale) while the market paths are NOT diverse: same two
//!    tokens, same sizes, same route. Nothing here is a diverse candidate mix.
//! 3. **The latency is modeled.** [`StorageLatency`] prices dispatched
//!    packets, not node work; the report says so in every table caption.
//!
//! # The legs
//!
//! - [`Leg::OffLegacy`] — the shipped posture: a session warm cache WITHOUT the
//!   delta opt-in, so the fan-out pays the anchor identity read and its ONE
//!   storage prefetch batch and requests no BAL anywhere.
//! - [`Leg::ReadyCurrent`] — the delta opt-in, with the session's canonical-head
//!   pipeline having settled the previous head and the current one BEFORE the
//!   fan-out: the fan-out reads the current generation and pays no prefetch.
//! - [`Leg::ReadyPrevious`] — the same two settled heads, with the fan-out
//!   anchored one block behind, which is the whole reason the driver keeps one
//!   previous generation.
//! - [`Leg::ReadyCurrentUnknown`] — the delta opt-in with the current head
//!   settled from a list that deliberately OMITS the candidate's read identity:
//!   the unknown-read control. The generation answers everything else, so the
//!   frame's read is the one dynamic fallback this point pays.
//! - [`Leg::ColdUnprepared`] — the delta opt-in with the current head DELIVERED
//!   but not yet applied: the control. It must take the legacy lane, pay the
//!   same prefetch batch, and request no BAL on the dispatch leg.
//! - [`Leg::ColdUndelivered`] — the same control with no head delivered at all.
//!
//! # The candidate read, and the two words
//!
//! Every simulated candidate executes the same scripted frame against the
//! executor's own storage: one read of the executor's [`READ_SLOT`] word, and
//! its two arms are the fixture's value sensitivity — the zero word
//! takes the minimal-retained arm (a 1-wei receipt, priced `gas_unprofitable`)
//! and the nonzero word returns the word itself (a `gas_profitable` receipt).
//! Each point therefore runs TWO words (zero and nonzero) over the same
//! candidates, head, deployment and provider state, so a lane that served a
//! wrong value would land in the other arm and its verdict rows would diverge.
//! The witness is the candidate's own post-execution access list
//! ([`RunMeasure::read_rows`]): the production capture records every slot the
//! frame loaded, so the read is asserted from the settled execution, never from
//! the fact that the corpus contains bytecode that could read.
//!
//! # The generation's content
//!
//! The list each head's prepare fetches is built from the PRODUCTION prefetch's
//! own key set: one discovery fan-out on the legacy lane records the storage
//! identities the derived batch named, and those exact identities become the
//! block's final writes (valued as the chain serves them, zero). The COVERED
//! list adds exactly one identity — the candidate's read key, valued as the
//! point's scripted word — and the UNKNOWN list omits it, so the two ready
//! controls differ in one list entry and nothing else. The ready generation
//! therefore carries every identity the legacy lane would have prefetched plus
//! (when covered) the candidate's own read, which is what makes "a candidate
//! reading them pays no dynamic fallback read" a checkable claim instead of an
//! assumption: [`Generation::identities_covered`] and
//! [`Generation::read_value`] are those checks. The discovery fan-out is setup:
//! it is never timed and its wire is discarded.
//!
//! # Background BAL cost
//!
//! The prepare (fetch + full decode) and apply (the one synchronous transition)
//! walls are measured per settled head and reported in their own table. They
//! are the reason the fan-out pays nothing, and they are NEVER subtracted from
//! a fan-out wall: the two are different clocks, and the report keeps them
//! apart.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alloy::primitives::{address, Address, Bytes, B256, U256};
use alloy::rpc::types::AccessList;
use alloy_eip7928::bal::Bal;
use alloy_eip7928::{AccountChanges, BlockAccessIndex, SlotChanges, StorageChange};

use degenbot_arbitrage::{
    dispatch_profitable_results, BlockPriorityFees, CmdExecutorBinding, DispatchCandidate,
    DispatchOutcome, FeeOnTransferRegistry, PoolDivergence, SimulateContext, SimulationBlock,
    SimulationDeployment, SolveStep,
};
use degenbot_execution::{CandidateId, DeploymentId, HopInfo, PathInfo, V2HopInfo};
use degenbot_executor::composers::EncodeOptions;
use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm2::delta_snapshot::{BalHeadDriver, HeadOutcome};
use degenbot_simulation::sim::evm2::warm_code_cache::{
    select_dispatch_storage, DeltaDispatchConfig, WarmCodeCacheInner,
};
use degenbot_simulation::sim::prefetch::{path_relevance_addresses, PrefetchFacts};
use degenbot_simulation::sim::{sim_metrics, ReadAnchor};
use degenbot_submission::PathSuppression;
use degenbot_substrate::state_lock::StateLock;
use degenbot_substrate::BotState;

use crate::probe::{DeltaRecorder, StorageLatency, WireCensus, BLOCK};

/// The owner + executor + settlement identities the built-in `cmd_executor`
/// deployment declares (the same scripted facts the dispatch unit suites use).
const OWNER: Address = address!("9c56a29c7231974c269e24f9fb3c29203039089e");
/// The deployment's executor: the address the injected runtime executes at, so
/// it is also the account every candidate frame reads storage from.
pub const EXECUTOR: Address = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
const WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
const PM: Address = address!("000000000004444c5dc75cb358380d2e3de08a90");
/// The one non-settlement token every candidate trades through.
const TOKEN: Address = address!("1111111111111111111111111111111111111111");

/// `BLOCK - 2`: the head whose settled generation the previous head carries.
const GRANDPARENT: B256 = B256::new([0xa1; 32]);
/// `BLOCK - 1`: the previous head, the anchor a lagging fan-out reads.
const PREVIOUS: B256 = B256::new([0xb1; 32]);
/// `BLOCK`: the head every on-time fan-out reads.
const CURRENT: B256 = B256::new([0xc1; 32]);
/// The parent of `GRANDPARENT`, so the first settled head is the one that
/// cannot carry.
const GENESIS: B256 = B256::new([0xa0; 32]);

/// The readiness deadline every delta leg arms. Irrelevant to a fan-out (the
/// selection never waits) and generous enough that the scripted chain settles
/// every head well inside it.
const READINESS_DEADLINE: Duration = Duration::from_millis(500);

/// The executor's own storage slot every candidate frame reads — the word the
/// fixture makes value-sensitive. It is deliberately NOT path-derivable, so the
/// legacy prefetch cannot know it and the only thing that can serve it with no
/// dynamic read is a generation whose list carried it.
pub const READ_SLOT: U256 = U256::from_limbs([0x5eed, 0, 0, 0]);

/// The word the NONZERO run scripts: one WETH-sized figure, so the nonzero arm's
/// receipt clears gas and the zero arm's 1-wei receipt does not.
pub const NONZERO_WORD: U256 = U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]);

/// The storage identity every candidate frame reads: the executor's own
/// [`READ_SLOT`] word.
#[must_use]
pub const fn read_identity() -> (Address, U256) {
    (EXECUTOR, READ_SLOT)
}

/// One scripted storage word the frame reads: the fixture's value-sensitivity
/// dimension. Two words run over the same candidates, head, deployment and
/// provider state, so a lane serving the wrong value lands in the other arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptedWord {
    /// The zero word: the frame takes its minimal-retained arm.
    Zero,
    /// One WETH-sized word: the frame returns it and prices `gas_profitable`.
    Nonzero,
}

impl ScriptedWord {
    /// Every word, in report order.
    pub const ALL: [Self; 2] = [Self::Zero, Self::Nonzero];

    /// The word the chain serves and the covered list carries.
    #[must_use]
    pub const fn value(self) -> U256 {
        match self {
            Self::Zero => U256::ZERO,
            Self::Nonzero => NONZERO_WORD,
        }
    }

    /// The report's name for this word.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Zero => "zero",
            Self::Nonzero => "nonzero",
        }
    }
}

/// The minimal value-sensitive storage-reading executor runtime the deployment
/// declares at [`EXECUTOR`].
///
/// It is the whole candidate frame: read [`READ_SLOT`] from the executor's own
/// storage, branch on the word, and RETURN a two-word `execute` receipt
/// (`gross_profit`, `bribe_paid`) — the shape the built-in adapter decodes.
///
/// - word `!= 0` — the nonzero arm returns the word itself as `gross_profit`.
/// - word `== 0` — the minimal-retained arm returns 1 wei, which prices
///   `gas_unprofitable`. A zero receipt would be a `no-profit` decline, and a
///   declined candidate carries no post-execution access list to witness the
///   read; 1 wei keeps the zero run's read observable as a verdict ROW instead.
///
/// Both arms therefore cost one SLOAD, and which arm runs is decided by the
/// stored value — a wrong word moves the candidate's output category.
#[must_use]
pub fn read_runtime() -> Bytes {
    let mut code: Vec<u8> = Vec::new();
    code.push(0x7f); // PUSH32 <slot>
    code.extend_from_slice(&READ_SLOT.to_be_bytes::<32>());
    code.push(0x54); // SLOAD -> [word]
    code.push(0x60); // PUSH1 <nonzero arm>
    let dest_at = code.len();
    code.push(0x00); // patched once the arm offset is known
    code.push(0x57); // JUMPI: the branch is the stored word
                     // Zero arm: receipt(1, 0).
    code.extend_from_slice(&[0x60, 0x01, 0x60, 0x00, 0x52]); // PUSH1 1; PUSH1 0; MSTORE
    code.extend_from_slice(&[0x60, 0x00, 0x60, 0x20, 0x52]); // PUSH1 0; PUSH1 32; MSTORE
    code.extend_from_slice(&[0x60, 0x40, 0x60, 0x00, 0xf3]); // PUSH1 64; PUSH1 0; RETURN
    code.push(0x5b); // JUMPDEST: the nonzero arm starts here
    let dest =
        u8::try_from(code.len() - 1).expect("the nonzero arm's JUMPDEST fits one PUSH1 offset");
    code[dest_at] = dest;
    // Nonzero arm: receipt(word, 0) — re-read the slot, since the branch
    // consumed the loaded word as its condition.
    code.push(0x7f);
    code.extend_from_slice(&READ_SLOT.to_be_bytes::<32>());
    code.push(0x54); // SLOAD -> [word]
    code.extend_from_slice(&[0x60, 0x00, 0x52]); // PUSH1 0; MSTORE
    code.extend_from_slice(&[0x60, 0x40, 0x60, 0x00, 0xf3]); // PUSH1 64; PUSH1 0; RETURN
    Bytes::from(code)
}

/// One lane of the comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leg {
    /// The shipped posture: a session cache without the delta opt-in.
    OffLegacy,
    /// The delta opt-in with the current head already settled, its list
    /// carrying the candidate's read identity.
    ReadyCurrent,
    /// The delta opt-in with the previous head still inside the read window.
    ReadyPrevious,
    /// The delta opt-in with the current head settled from a list that OMITS
    /// the candidate's read identity: the unknown-read control.
    ReadyCurrentUnknown,
    /// The delta opt-in with the current head delivered but not applied.
    ColdUnprepared,
    /// The delta opt-in with no head delivered at all.
    ColdUndelivered,
}

impl Leg {
    /// Every leg, in the order a repetition interleaves them.
    pub const ALL: [Self; 6] = [
        Self::OffLegacy,
        Self::ReadyCurrent,
        Self::ReadyPrevious,
        Self::ReadyCurrentUnknown,
        Self::ColdUnprepared,
        Self::ColdUndelivered,
    ];

    /// Whether this leg settles a ready generation before the fan-out.
    #[must_use]
    pub const fn is_ready(self) -> bool {
        matches!(
            self,
            Self::ReadyCurrent | Self::ReadyPrevious | Self::ReadyCurrentUnknown
        )
    }

    /// The report's name for this leg.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::OffLegacy => "off-legacy",
            Self::ReadyCurrent => "on-ready-current",
            Self::ReadyPrevious => "on-ready-previous",
            Self::ReadyCurrentUnknown => "on-ready-unknown",
            Self::ColdUnprepared => "on-cold-unprepared",
            Self::ColdUndelivered => "on-cold-undelivered",
        }
    }
}

/// The scripted candidate corpus at one fan-out size.
#[derive(Clone)]
pub struct Corpus {
    /// How many candidates the caller hands the fan-out (the production cap
    /// applies downstream, so `candidates.len()` may exceed what it simulates).
    pub count: usize,
    /// The candidates, in the caller's order (engine profit descending is the
    /// caller's contract, and every candidate here carries the same profit, so
    /// the order is the id order).
    pub candidates: Vec<DispatchCandidate>,
    /// The production pre-simulation relevance set over these paths and the
    /// deployment's settlement declaration — the set a head's apply filters its
    /// writes through.
    pub relevance: Vec<Address>,
    /// The distinct pool identities the corpus names, two per candidate.
    pub pools: Vec<Address>,
}

impl Corpus {
    /// The same candidates one block behind: the anchor a lagging fan-out reads.
    #[must_use]
    pub fn lagged(&self) -> Vec<DispatchCandidate> {
        self.candidates
            .iter()
            .map(|candidate| {
                let mut lagged = candidate.clone();
                lagged.solve_block = BLOCK - 1;
                lagged
            })
            .collect()
    }
}

/// The built-in deployment identity every leg injects.
fn deployment_id() -> DeploymentId {
    DeploymentId::new(1)
}

/// The built-in `cmd_executor` binding every fan-out test injects.
fn binding() -> CmdExecutorBinding {
    CmdExecutorBinding::new(
        OWNER,
        EXECUTOR,
        WETH,
        PM,
        EncodeOptions {
            erc6909_profit: false,
            use_v4_batch: false,
            ..EncodeOptions::default()
        },
    )
}

/// The deployment with the value-sensitive reader ([`read_runtime`]) installed
/// at the executor address: every candidate frame reads the scripted word and
/// returns a receipt that depends on it.
fn deployment() -> SimulationDeployment {
    binding()
        .injected(deployment_id(), EXECUTOR, read_runtime())
        .expect("the declared override names the executor")
}

/// Candidate `index`'s `which`th pool: a deterministic address no other
/// candidate shares, so the derived prefetch set (and the BAL list) scales with
/// the corpus instead of collapsing onto shared pools.
fn pool_address(index: usize, which: u8) -> Address {
    let mut bytes = [0u8; 20];
    let index = u64::try_from(index).expect("the corpus index fits in u64");
    bytes[0] = if which == 0 { 0x50 } else { 0x51 };
    bytes[2..10].copy_from_slice(&index.to_be_bytes());
    bytes[19] = 0xee;
    Address::new(bytes)
}

/// One two-hop WETH -> TOKEN -> WETH candidate through its own pool pair.
fn candidate(index: usize) -> DispatchCandidate {
    let (first, second) = (pool_address(index, 0), pool_address(index, 1));
    let path = || {
        PathInfo::new(vec![
            HopInfo::V2(V2HopInfo {
                pool_address: first,
                token0_address: WETH,
                token1_address: TOKEN,
                fee: 30,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: second,
                token0_address: TOKEN,
                token1_address: WETH,
                fee: 30,
                zfo: true,
            }),
        ])
    };
    let optimal_input = 1_000_000_000_000_000_000u128;
    DispatchCandidate {
        candidate_id: CandidateId::new(
            u64::try_from(index).expect("the corpus index fits in u64"),
            deployment_id(),
        ),
        optimal_input,
        engine_profit: 1_000,
        steps: Box::new([
            SolveStep {
                output: optimal_input * 11 / 10,
                consumed_input: optimal_input,
                state_nonce: 0,
            },
            SolveStep {
                output: optimal_input * 121 / 100,
                consumed_input: optimal_input * 11 / 10,
                state_nonce: 0,
            },
        ]),
        solve_block: BLOCK,
        path_info: path(),
        prepared: None,
    }
}

/// The corpus at `count` candidates: distinct pool identities, one route shape.
///
/// The relevance set is the production derivation (each path's pools, both
/// sides of each pair, the settlement identities) PLUS one declared address: the
/// executor whose own storage the scripted frame reads. A path derivation has no
/// word for a frame's own storage — the real executor reads its balances at WETH
/// and the `PoolManager` — so the reader's own account is the one fixture fact this
/// corpus declares, and it is what makes the covered/unknown controls a
/// difference in the LIST rather than in the relevance policy.
#[must_use]
pub fn corpus(count: usize) -> Corpus {
    let candidates: Vec<DispatchCandidate> = (0..count).map(candidate).collect();
    let declaration = deployment();
    let mut relevance = path_relevance_addresses(
        candidates.iter().map(|candidate| &candidate.path_info),
        &PrefetchFacts {
            settlement: declaration.settlement(),
        },
    );
    if !relevance.contains(&EXECUTOR) {
        relevance.push(EXECUTOR);
    }
    let pools = (0..count)
        .flat_map(|index| [pool_address(index, 0), pool_address(index, 1)])
        .collect();
    Corpus {
        count,
        candidates,
        relevance,
        pools,
    }
}

/// The raw EIP-7928 list carrying `writes` as one `block_access_index = 0`
/// storage change each, at the value the run scripts for that identity.
#[must_use]
pub fn bal_bytes(writes: &[((Address, U256), U256)]) -> Vec<u8> {
    let mut accounts: Vec<AccountChanges> = Vec::new();
    for &((address, slot), new_value) in writes {
        let entry = SlotChanges {
            slot,
            changes: vec![StorageChange {
                block_access_index: BlockAccessIndex(0),
                new_value,
            }],
        };
        match accounts
            .iter_mut()
            .find(|account| account.address == address)
        {
            Some(account) => account.storage_changes.push(entry),
            None => accounts.push(AccountChanges {
                address,
                storage_changes: vec![entry],
                storage_reads: Vec::new(),
                balance_changes: Vec::new(),
                nonce_changes: Vec::new(),
                code_changes: Vec::new(),
            }),
        }
    }
    alloy::rlp::encode(Bal::from(accounts))
}

/// The session-scoped handles one fan-out mutates. Fresh per run, so one leg's
/// suppression/divergence feedback can never reach another's.
struct Filters {
    suppression: Arc<Mutex<PathSuppression>>,
    divergence: Arc<Mutex<PoolDivergence>>,
    fot: Arc<Mutex<FeeOnTransferRegistry>>,
    bot_state: Arc<StateLock<BotState>>,
}

impl Filters {
    fn fresh() -> Self {
        Self {
            suppression: Arc::new(Mutex::new(PathSuppression::new())),
            divergence: Arc::new(Mutex::new(PoolDivergence::new())),
            fot: Arc::new(Mutex::new(FeeOnTransferRegistry::new())),
            bot_state: Arc::new(StateLock::new(BotState::new())),
        }
    }
}

/// One fan-out through the production entry point, over `cache`.
fn fan_out(
    provider: &AlloyProvider,
    deployment: &SimulationDeployment,
    candidates: &[DispatchCandidate],
    cache: Option<Arc<parking_lot::RwLock<WarmCodeCacheInner>>>,
) -> DispatchOutcome {
    let filters = Filters::fresh();
    let block = SimulationBlock {
        provider,
        base_fee_next: 1_000_000_000u128,
        current_block: BLOCK,
        block_timestamp: 0,
        block_priority_fees: Some(BlockPriorityFees {
            block: BLOCK,
            p10: U256::from(500_000_000u64),
            p50: U256::from(2_000_000_000u64),
        }),
    };
    let ctx = SimulateContext { block, deployment };
    dispatch_profitable_results(
        candidates.to_vec(),
        &ctx,
        &filters.suppression,
        BLOCK,
        0,
        &filters.divergence,
        &filters.fot,
        Some(filters.bot_state),
        cache,
    )
}

/// One candidate's verdict: the bucket that claimed it, or the priced figures
/// that decided it — plus the post-execution access list the frame's own
/// storage loads left behind. Two lanes agree only when every row matches.
#[must_use]
pub fn verdicts(outcome: &DispatchOutcome) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = outcome
        .gas_profitable
        .iter()
        .map(|result| {
            (
                result.candidate_id.to_string(),
                format!(
                    "profitable gross={} net={} gas={} passed={} reads={}",
                    result.gross_profit,
                    result.net_profit,
                    result.gas_used,
                    result.passed,
                    read_set(result.access_list.as_ref())
                ),
            )
        })
        .chain(outcome.gas_unprofitable.iter().map(|result| {
            (
                result.candidate_id.to_string(),
                format!(
                    "unprofitable gross={} net={} gas={} passed={} reads={}",
                    result.gross_profit,
                    result.net_profit,
                    result.gas_used,
                    result.passed,
                    read_set(result.access_list.as_ref())
                ),
            )
        }))
        .chain(
            outcome
                .failures
                .iter()
                .map(|failure| (failure.candidate_id.to_string(), failure.bucket.clone())),
        )
        .collect();
    rows.sort();
    rows
}

/// The storage identities a candidate's post-execution access list names, as
/// `<address>:<slot>` strings, ascending. Empty when the frame recorded none.
#[must_use]
pub fn read_set(access_list: Option<&AccessList>) -> String {
    let mut reads: Vec<String> = access_list
        .map(|list| {
            list.0
                .iter()
                .flat_map(|item| {
                    item.storage_keys
                        .iter()
                        .map(|slot| format!("{}:{}", item.address, slot))
                        .collect::<Vec<String>>()
                })
                .collect()
        })
        .unwrap_or_default();
    reads.sort();
    format!("[{}]", reads.join(","))
}

/// Whether an access list names the storage identity `(address, slot)`.
#[must_use]
pub fn names_read(access_list: &AccessList, (address, slot): (Address, U256)) -> bool {
    access_list.0.iter().any(|item| {
        item.address == address
            && item
                .storage_keys
                .iter()
                .any(|key| U256::from_be_slice(key.as_slice()) == slot)
    })
}

/// The failure-bucket histogram, sorted by bucket name.
#[must_use]
pub fn fail_buckets(outcome: &DispatchOutcome) -> Vec<(String, u64)> {
    outcome
        .fail_buckets
        .buckets()
        .iter()
        .map(|(bucket, count)| (bucket.clone(), *count))
        .collect()
}

/// What one leg's background head pipeline did, per settled head.
#[derive(Debug, Clone, Default)]
pub struct Generation {
    /// The anchors the pipeline settled, in settlement order.
    pub anchors: Vec<u64>,
    /// The prepare wall (fetch + full decode) of each settled head, in us.
    pub prepare_us: Vec<u64>,
    /// The apply wall (the one synchronous transition) of each head, in us.
    pub apply_us: Vec<u64>,
    /// The final writes each head's report applied.
    pub writes: Vec<usize>,
    /// The writes each head's report skipped: address outside relevance.
    pub writes_skipped: Vec<usize>,
    /// The writes each head's report overwrote on a known key.
    pub writes_overwritten: Vec<usize>,
    /// The BAL identities the settled generation answers with, when it settled.
    pub hub_entries: usize,
    /// How many of the legacy prefetch's identities the generation holds.
    pub identities_covered: usize,
    /// How many identities were checked (the legacy prefetch's own key set).
    pub identities_checked: usize,
    /// The word the settled generation serves for the candidate's read
    /// identity, or `None` when its list never carried that key — the
    /// covered/unknown distinction as a checked fact.
    pub read_value: Option<U256>,
}

/// One run of one leg at one count.
#[derive(Debug, Clone)]
pub struct RunMeasure {
    /// Which leg this run is.
    pub leg: Leg,
    /// The fan-out's own wall, in microseconds.
    pub wall_us: u64,
    /// What the fan-out dispatched, as the transport recorded it.
    pub census: WireCensus,
    /// The wall those dispatched packets pay under the point's model.
    pub modeled_us: u64,
    /// The storage lane the fan-out's own selection resolved, and its reason.
    pub lane: String,
    /// The candidates the fan-out actually simulated (post-cap).
    pub simulated: usize,
    /// Process-global simulation calls this fan-out made.
    pub sim_calls: u64,
    /// Process-global EVM handle builds this fan-out made.
    pub handle_builds: u64,
    /// Every candidate's verdict row.
    pub verdicts: Vec<(String, String)>,
    /// The failure-bucket histogram.
    pub fail_buckets: Vec<(String, u64)>,
    /// The candidate count / failure / exception tallies.
    pub candidate_count: usize,
    /// The failure tally.
    pub fail_count: usize,
    /// The exception tally.
    pub exception_count: usize,
    /// How many candidates returned a submittable row (`gas_profitable`).
    pub profitable_rows: usize,
    /// How many candidates returned a result row (the only surface that carries
    /// a post-execution access list).
    pub result_rows: usize,
    /// How many of those rows names the corpus's read identity in their access
    /// list: the witness that the candidate actually executed its SLOAD.
    pub read_rows: usize,
    /// The background head pipeline's work, for a delta leg.
    pub generation: Option<Generation>,
}

impl RunMeasure {
    /// Whether this run's verdict surface is identical to `other`'s.
    #[must_use]
    pub fn same_verdicts(&self, other: &Self) -> bool {
        self.verdicts == other.verdicts && self.fail_buckets == other.fail_buckets
    }
}

/// A leg's census is a delta over the recorder's cumulative counters.
fn census_delta(before: WireCensus, after: WireCensus) -> WireCensus {
    WireCensus {
        header_reads: after.header_reads.saturating_sub(before.header_reads),
        storage_batches: after.storage_batches.saturating_sub(before.storage_batches),
        storage_members: after.storage_members.saturating_sub(before.storage_members),
        storage_singles: after.storage_singles.saturating_sub(before.storage_singles),
        bal_fetches: after.bal_fetches.saturating_sub(before.bal_fetches),
        other_requests: after.other_requests.saturating_sub(before.other_requests),
        packets: after.packets.saturating_sub(before.packets),
    }
}

/// A fresh delta session cache: the opt-in on, nothing delivered, prepared or
/// fetched yet.
fn delta_cache() -> Arc<parking_lot::RwLock<WarmCodeCacheInner>> {
    WarmCodeCacheInner::shared_with_delta_dispatch(DeltaDispatchConfig::with_readiness_deadline(
        READINESS_DEADLINE,
    ))
}

/// Arm the session's canonical-head pipeline and return its driver — the same
/// two calls the bot's own wiring makes.
fn arm(
    cache: &Arc<parking_lot::RwLock<WarmCodeCacheInner>>,
    provider: &AlloyProvider,
) -> Arc<BalHeadDriver> {
    let pipeline = cache
        .read()
        .bal_head_pipeline()
        .expect("the opt-in is on, so the session owns one pipeline");
    cache.read().arm_bal_heads(provider);
    pipeline.driver().expect("arming installs the driver")
}

/// Deliver, prepare and apply ONE head the way the canonical-head pipeline does
/// on the pump's header edge, timing the two stages apart.
///
/// # Errors
///
/// A head that does not settle (no lineage to carry, an unreadable list) is not
/// a head this benchmark can price a ready lane against.
async fn settle(
    driver: &Arc<BalHeadDriver>,
    number: u64,
    hash: B256,
    parent: B256,
    relevance: &[Address],
    generation: &mut Generation,
) -> Result<(), String> {
    let anchor = ReadAnchor::identified(number, hash);
    let ticket = driver.deliver(anchor, parent);
    let started = Instant::now();
    let prepared = driver.prepare(ticket).await;
    let prepare_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
    let started = Instant::now();
    let outcome = driver.apply(ticket, &prepared, relevance);
    let apply_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
    match outcome {
        HeadOutcome::Settled(report) => {
            generation.anchors.push(number);
            generation.prepare_us.push(prepare_us);
            generation.apply_us.push(apply_us);
            generation.writes.push(report.writes.len());
            generation.writes_skipped.push(report.writes_skipped);
            generation
                .writes_overwritten
                .push(report.writes_overwritten);
            Ok(())
        }
        other => Err(format!(
            "head {number} did not settle: {other:?} (prepare {prepare_us}us, apply {apply_us}us)"
        )),
    }
}

/// The lane label a fan-out's own selection resolves after the run, queried
/// through the production selector (so the label is the production
/// vocabulary, never the bench's guess).
fn lane_label(
    provider: &AlloyProvider,
    cache: &Arc<parking_lot::RwLock<WarmCodeCacheInner>>,
    anchor: u64,
) -> String {
    let selection = select_dispatch_storage(provider, cache, anchor);
    match selection.outcome.reason() {
        Some(reason) => format!("{}/{reason}", selection.outcome.mode()),
        None => selection.outcome.mode().to_string(),
    }
}

/// One leg's full run: setup, the measured fan-out, and its background
/// generation report.
///
/// # Errors
///
/// A scripted chain that cannot settle a head, or a session that cannot arm its
/// pipeline, is a broken fixture, never a result.
pub async fn run_leg(
    leg: Leg,
    corpus: &Corpus,
    recorder: &DeltaRecorder,
    provider: &AlloyProvider,
    deployment: &SimulationDeployment,
    plan: &GenerationPlan,
    model: &StorageLatency,
) -> Result<RunMeasure, String> {
    let lagging = matches!(leg, Leg::ReadyPrevious);
    let candidates = if lagging {
        corpus.lagged()
    } else {
        corpus.candidates.clone()
    };
    let anchor = if lagging { BLOCK - 1 } else { BLOCK };
    // The list every head of THIS leg carries: the covered or the unknown one.
    recorder.set_bal_payload(bal_bytes(plan.writes_for(leg)));
    let (cache, generation) = prepare_leg(leg, corpus, provider, plan, anchor, lagging).await?;
    let session = Session {
        provider,
        deployment,
        cache,
        anchor,
        latency: *model,
    };
    Ok(measure_leg(
        leg,
        &candidates,
        recorder,
        &session,
        generation,
    ))
}

/// What one leg's measured fan-out runs against: the provider the chain answers
/// through, the deployment declaration, the session cache the fan-out reads
/// through, the anchor it resolves, and the latency model its wire pays.
struct Session<'a> {
    provider: &'a AlloyProvider,
    deployment: &'a SimulationDeployment,
    cache: Arc<parking_lot::RwLock<WarmCodeCacheInner>>,
    anchor: u64,
    latency: StorageLatency,
}

/// One point's two BAL write sets and the production prefetch key set they must
/// keep covering.
///
/// The COVERED list is the discovery fan-out's identities (valued as the chain
/// serves them) plus the candidate's read identity at the point's word; the
/// UNKNOWN list is the same list with that one entry omitted. The two differ in
/// exactly one list entry, which is what makes the pair a controlled comparison
/// of covered versus dynamic-fallback reads.
#[derive(Clone, Debug)]
pub struct GenerationPlan {
    /// The identities the legacy prefetch derived: what every ready generation
    /// must answer.
    pub prefetch: Vec<(Address, U256)>,
    /// `prefetch` plus the read identity, valued at the point's word.
    pub covered: Vec<((Address, U256), U256)>,
    /// `prefetch` alone: the read identity deliberately omitted.
    pub unknown: Vec<((Address, U256), U256)>,
}

impl GenerationPlan {
    /// The list a leg's settled heads serve.
    #[must_use]
    pub fn writes_for(&self, leg: Leg) -> &[((Address, U256), U256)] {
        match leg {
            Leg::ReadyCurrentUnknown => &self.unknown,
            _ => &self.covered,
        }
    }
}

/// The leg's session setup: the cache the fan-out reads through and, for a ready
/// leg, what the background head pipeline settled and the generation it left for
/// the fan-out to read.
///
/// # Errors
///
/// A head that does not settle is a broken fixture, never a result.
async fn prepare_leg(
    leg: Leg,
    corpus: &Corpus,
    provider: &AlloyProvider,
    plan: &GenerationPlan,
    anchor: u64,
    lagging: bool,
) -> Result<
    (
        Arc<parking_lot::RwLock<WarmCodeCacheInner>>,
        Option<Generation>,
    ),
    String,
> {
    let mut generation: Option<Generation> = None;
    let cache: Arc<parking_lot::RwLock<WarmCodeCacheInner>> = match leg {
        Leg::OffLegacy => WarmCodeCacheInner::shared_default(),
        Leg::ColdUndelivered => delta_cache(),
        Leg::ColdUnprepared => {
            let cache = delta_cache();
            let driver = arm(&cache, provider);
            // The head arrived on the block clock; nothing prepared it.
            let _ticket = driver.deliver(ReadAnchor::identified(BLOCK, CURRENT), PREVIOUS);
            cache
        }
        Leg::ReadyCurrent | Leg::ReadyPrevious | Leg::ReadyCurrentUnknown => {
            let cache = delta_cache();
            let driver = arm(&cache, provider);
            let mut settled = Generation::default();
            for (number, hash, parent) in [
                (BLOCK - 2, GRANDPARENT, GENESIS),
                (BLOCK - 1, PREVIOUS, GRANDPARENT),
                (BLOCK, CURRENT, PREVIOUS),
            ] {
                settle(
                    &driver,
                    number,
                    hash,
                    parent,
                    &corpus.relevance,
                    &mut settled,
                )
                .await?;
            }
            // What the generation the fan-out will read actually holds: every
            // identity the legacy lane's prefetch derived must be answerable
            // here, which is what makes a dynamic fallback read impossible for
            // a candidate reading those words.
            let view = driver.snapshot(ReadAnchor::identified(
                anchor,
                if lagging { PREVIOUS } else { CURRENT },
            ));
            settled.hub_entries = view.hub.snapshot_len();
            settled.identities_checked = plan.prefetch.len();
            settled.identities_covered = plan
                .prefetch
                .iter()
                .filter(|&&(address, slot)| view.hub.snapshot_value(address, slot).is_some())
                .count();
            // The candidate's own read: present with this point's word on a
            // covered list, absent on the unknown one.
            let (address, slot) = read_identity();
            settled.read_value = view.hub.snapshot_value(address, slot);
            generation = Some(settled);
            cache
        }
    };
    Ok((cache, generation))
}

/// The measured fan-out: the ONE production dispatch entry over the candidates
/// and the cache the leg prepared, its wire, and the background work that made
/// the cache what it is.
fn measure_leg(
    leg: Leg,
    candidates: &[DispatchCandidate],
    recorder: &DeltaRecorder,
    session: &Session<'_>,
    generation: Option<Generation>,
) -> RunMeasure {
    recorder.clear_wire();
    let before = recorder.census();
    let sims_before = sim_metrics::snapshot();
    let started = Instant::now();
    let outcome = fan_out(
        session.provider,
        session.deployment,
        candidates,
        Some(Arc::clone(&session.cache)),
    );
    let wall_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
    let sims_after = sim_metrics::snapshot();
    let census = census_delta(before, recorder.census());
    let lane = lane_label(session.provider, &session.cache, session.anchor);
    let sims = sim_metrics::delta(sims_after, sims_before);
    // The post-execution read witness: only a result row carries an access
    // list, so the tally is (rows returned, rows that name the read identity).
    let mut result_rows = 0usize;
    let mut read_rows = 0usize;
    for result in outcome
        .gas_profitable
        .iter()
        .chain(outcome.gas_unprofitable.iter())
    {
        result_rows += 1;
        if result
            .access_list
            .as_ref()
            .is_some_and(|list| names_read(list, read_identity()))
        {
            read_rows += 1;
        }
    }
    RunMeasure {
        leg,
        wall_us,
        modeled_us: census.modeled_us(&session.latency),
        census,
        lane,
        simulated: outcome.candidate_count,
        sim_calls: sims.sim_calls,
        handle_builds: sims.handle_builds,
        verdicts: verdicts(&outcome),
        fail_buckets: fail_buckets(&outcome),
        candidate_count: outcome.candidate_count,
        fail_count: outcome.fail_count,
        exception_count: outcome.exception_count,
        profitable_rows: outcome.gas_profitable.len(),
        result_rows,
        read_rows,
        generation,
    }
}

/// The whole matrix.
#[derive(Clone, Default)]
pub struct Bench {
    /// Every (count, word, model) point.
    pub points: Vec<Point>,
}

/// One point of the matrix: a count, a scripted word and a latency model, every
/// leg, every rep.
#[derive(Clone)]
pub struct Point {
    /// The caller's candidate count.
    pub count: usize,
    /// The storage word every frame of this point read.
    pub word: ScriptedWord,
    /// The model every packet of this point paid.
    pub model: StorageLatency,
    /// The distinct storage identities the legacy prefetch derived.
    pub prefetch_identities: usize,
    /// The distinct pool identities the corpus names.
    pub pool_identities: usize,
    /// The production cap: what the fan-out actually simulated.
    pub simulated: usize,
    /// Every run of every leg, in interleaved repetition order.
    pub runs: Vec<RunMeasure>,
}

impl Point {
    /// The runs of one leg, in repetition order.
    #[must_use]
    pub fn leg(&self, leg: Leg) -> Vec<&RunMeasure> {
        self.runs.iter().filter(|run| run.leg == leg).collect()
    }

    /// Every leg's verdict surface is `OffLegacy`'s.
    #[must_use]
    pub fn parity(&self) -> bool {
        let Some(reference) = self.leg(Leg::OffLegacy).first().copied() else {
            return false;
        };
        self.runs.iter().all(|run| run.same_verdicts(reference))
    }

    /// One leg's summary, or `None` when the leg did not run.
    #[must_use]
    pub fn summary(&self, leg: Leg) -> Option<LegSummary> {
        let runs = self.leg(leg);
        let first = runs.first()?;
        let column = |pick: fn(&RunMeasure) -> u64| {
            median(&runs.iter().map(|run| pick(run)).collect::<Vec<_>>())
        };
        Some(LegSummary {
            median: column(|run| run.wall_us),
            min: runs.iter().map(|run| run.wall_us).min().unwrap_or(0),
            max: runs.iter().map(|run| run.wall_us).max().unwrap_or(0),
            members: column(|run| run.census.storage_members),
            batches: column(|run| run.census.storage_batches),
            singles: column(|run| run.census.storage_singles),
            bal: column(|run| run.census.bal_fetches),
            headers: column(|run| run.census.header_reads),
            packets: column(|run| run.census.packets),
            modeled: column(|run| run.modeled_us),
            sims: column(|run| run.sim_calls),
            builds: column(|run| run.handle_builds),
            profitable: column(|run| u64::try_from(run.profitable_rows).unwrap_or(u64::MAX)),
            result_rows: column(|run| u64::try_from(run.result_rows).unwrap_or(u64::MAX)),
            read_rows: column(|run| u64::try_from(run.read_rows).unwrap_or(u64::MAX)),
            read_covered: column(|run| {
                u64::from(
                    run.generation
                        .as_ref()
                        .is_some_and(|generation| generation.read_value.is_some()),
                )
            }),
            candidates: first.candidate_count,
            fails: first.fail_count,
            exceptions: first.exception_count,
            lane: first.lane.clone(),
        })
    }
}

/// Run the matrix: every count, every word, every model, `reps` interleaved
/// repetitions.
///
/// # Errors
///
/// A chain that cannot settle a head fails the whole bench: a point measured on
/// a broken fixture is not a result.
pub fn run(
    counts: &[usize],
    words: &[ScriptedWord],
    models: &[StorageLatency],
    reps: usize,
) -> Result<Bench, String> {
    let mut points = Vec::new();
    for &count in counts {
        for &word in words {
            for &model in models {
                points.push(run_point(count, word, model, reps)?);
            }
        }
    }
    Ok(Bench { points })
}

/// Run one point, on its own runtime and its own scripted chain.
///
/// # Errors
///
/// See [`run_leg`].
fn run_point(
    count: usize,
    word: ScriptedWord,
    model: StorageLatency,
    reps: usize,
) -> Result<Point, String> {
    let corpus = corpus(count);
    let runtime = crate::probe::runtime();
    runtime.block_on(async move {
        let recorder = DeltaRecorder::with_latency(model);
        recorder.serve_head(BLOCK - 2, GRANDPARENT, GENESIS);
        recorder.serve_head(BLOCK - 1, PREVIOUS, GRANDPARENT);
        recorder.serve_head(BLOCK, CURRENT, PREVIOUS);
        let provider = crate::probe::provider(&recorder);
        let declaration = deployment();

        // The one word this point scripts: what the chain answers for the
        // candidate's read identity, and what a covered list carries.
        recorder.serve_storage(EXECUTOR, READ_SLOT, word.value());

        // The discovery fan-out: the PRODUCTION prefetch derives the key set on
        // the legacy lane, and those exact identities become the block's final
        // writes. Setup only — never timed, its wire discarded. The frame's own
        // read reaches the node as ONE serial read on this lane; it is the
        // fixture's read identity, not a derived prefetch member, so it is
        // dropped from the derived set and re-added only to the covered list.
        recorder.clear_wire();
        let _discovery = fan_out(
            &provider,
            &declaration,
            &corpus.candidates,
            Some(WarmCodeCacheInner::shared_default()),
        );
        let mut seen = HashSet::new();
        let prefetch: Vec<(Address, U256)> = recorder
            .storage_identities()
            .into_iter()
            .filter(|key| *key != read_identity())
            .filter(|key| seen.insert(*key))
            .collect();
        let mut covered: Vec<((Address, U256), U256)> =
            prefetch.iter().map(|&key| (key, U256::ZERO)).collect();
        covered.push((read_identity(), word.value()));
        let unknown: Vec<((Address, U256), U256)> =
            prefetch.iter().map(|&key| (key, U256::ZERO)).collect();
        let plan = GenerationPlan {
            prefetch,
            covered,
            unknown,
        };

        let mut runs = Vec::new();
        for _rep in 0..reps {
            for leg in Leg::ALL {
                runs.push(
                    run_leg(
                        leg,
                        &corpus,
                        &recorder,
                        &provider,
                        &declaration,
                        &plan,
                        &model,
                    )
                    .await?,
                );
            }
        }
        let simulated = runs.first().map_or(0, |run| run.simulated);
        Ok::<Point, String>(Point {
            count: corpus.count,
            word,
            model,
            prefetch_identities: plan.prefetch.len(),
            pool_identities: corpus.pools.len(),
            simulated,
            runs,
        })
    })
}

/// The median of `values`.
#[must_use]
pub fn median(values: &[u64]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    sorted[sorted.len() / 2]
}

/// One leg's absolute row at one point: the median / min / max wall of its
/// repetitions and the wire that wall paid for. Every duration is microseconds.
#[derive(Debug, Clone)]
pub struct LegSummary {
    /// The median fan-out wall.
    pub median: u64,
    /// The fastest repetition.
    pub min: u64,
    /// The slowest repetition.
    pub max: u64,
    /// The median storage batch members dispatched.
    pub members: u64,
    /// The median storage batch packets dispatched.
    pub batches: u64,
    /// The median serial storage reads (dynamic fallbacks).
    pub singles: u64,
    /// The median BAL fetches on the DISPATCH leg.
    pub bal: u64,
    /// The median header reads.
    pub headers: u64,
    /// The median dispatched packets of every method (a batch counted as one).
    pub packets: u64,
    /// The model's wall for the median wire.
    pub modeled: u64,
    /// The median number of simulation calls the fan-out made.
    pub sims: u64,
    /// The median number of EVM handle builds the fan-out made.
    pub builds: u64,
    /// The median count of submittable (`gas_profitable`) rows.
    pub profitable: u64,
    /// The median count of result rows the fan-out returned.
    pub result_rows: u64,
    /// The median count of those rows carrying the read identity.
    pub read_rows: u64,
    /// The word the settled generation serves for the read identity: the median
    /// presence over the repetitions (`1` = covered, `0` = unknown).
    pub read_covered: u64,
    /// The candidate tally the fan-out categorized (post-cap).
    pub candidates: usize,
    /// The failure tally.
    pub fails: usize,
    /// The exception tally.
    pub exceptions: usize,
    /// The lane label the run's own selection resolved.
    pub lane: String,
}

/// The report: every table of the run, in the order a reader needs them.
#[must_use]
pub fn render(bench: &Bench) -> String {
    let mut out = String::new();
    render_corpus(bench, &mut out);
    render_absolute(bench, &mut out);
    render_verdicts(bench, &mut out);
    render_reads(bench, &mut out);
    render_comparison(bench, &mut out);
    render_controls(bench, &mut out);
    render_background(bench, &mut out);
    render_variability(bench, &mut out);
    out
}

/// The corpus-identity table: what the counts actually are.
fn render_corpus(bench: &Bench, out: &mut String) {
    let _ = write!(
        out,
        "## Corpus identity\n\n\
         One scripted chain (scripted storage words), one deployment declaration, one\n\
         candidate list per count. Candidate `i` names its own two pool identities, so the\n\
         corpus's distinct path identities equal the count while its route shape, sizes and\n\
         tokens are identical throughout — repeated market paths, never a diverse mix.\n\n\
         Every simulated candidate executes the SAME value-sensitive frame: one SLOAD of\n\
         the executor's own slot `0x5eed`, then a two-word `execute` receipt whose\n\
         `gross_profit` is the frame's answer to that word — the zero word takes the\n\
         minimal-retained arm (1 wei, priced `gas_unprofitable`), the nonzero word returns\n\
         the word itself (priced `gas_profitable`). Every point runs BOTH words, so the\n\
         storage value is a variable and a lane serving a wrong word moves the row.\n\n\
         | count | pool identities | prefetch identities | simulated (post-cap) |\n\
         | --- | --- | --- | --- |\n"
    );
    let mut seen: Vec<usize> = Vec::new();
    for point in &bench.points {
        if seen.contains(&point.count) {
            continue;
        }
        seen.push(point.count);
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            point.count, point.pool_identities, point.prefetch_identities, point.simulated
        );
    }
    out.push('\n');
}

/// The absolute table: every leg's wall and wire at every count and word.
fn render_absolute(bench: &Bench, out: &mut String) {
    for point in &bench.points {
        let _ = write!(
            out,
            "### count {} — word {} — model: {}\n\n\
             | leg | lane | median us | min | max | us/candidate | batches | members | singles | bal | headers | modeled us | candidates | results | sims | builds | fails | exceptions |\n\
             | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n",
            point.count,
            point.word.label(),
            point.model.label()
        );
        for leg in Leg::ALL {
            let Some(summary) = point.summary(leg) else {
                continue;
            };
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {:.1} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                leg.label(),
                summary.lane,
                summary.median,
                summary.min,
                summary.max,
                per_candidate(summary.median, summary.candidates),
                summary.batches,
                summary.members,
                summary.singles,
                summary.bal,
                summary.headers,
                summary.modeled,
                summary.candidates,
                summary.result_rows,
                summary.sims,
                summary.builds,
                summary.fails,
                summary.exceptions,
            );
        }
        let _ = write!(
            out,
            "\nparity: {}\n\n",
            if point.parity() {
                "every leg identical"
            } else {
                "MISMATCH"
            }
        );
    }
}

/// The verdict surface: the legacy reference's rows and the parity pin. The
/// point of the table is that the WORD decides the category — one figure per
/// (count, word) is the whole fan-out's verdict class.
fn render_verdicts(bench: &Bench, out: &mut String) {
    let _ = write!(
        out,
        "## Verdict surface\n\n\
         The legacy reference's rows (`profitable` = submittable, `unprofitable` = a\n\
         positive gross that did not clear gas, `failures` = a declined candidate) plus\n\
         the parity pin. The word decides the category: the zero word's 1-wei receipt can\n\
         never clear gas, the nonzero word's receipt always does.\n\n\
         | count | word | model | profitable | unprofitable | failures | failure buckets | parity |\n\
         | --- | --- | --- | --- | --- | --- | --- | --- |\n"
    );
    for point in &bench.points {
        let Some(reference) = point.summary(Leg::OffLegacy) else {
            continue;
        };
        let buckets: Vec<String> = point
            .leg(Leg::OffLegacy)
            .first()
            .map(|run| {
                run.fail_buckets
                    .iter()
                    .map(|(bucket, count)| format!("{bucket}:{count}"))
                    .collect()
            })
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {} |",
            point.count,
            point.word.label(),
            point.model.label(),
            reference.profitable,
            reference.result_rows - reference.profitable,
            reference.fails,
            if buckets.is_empty() {
                "none".to_string()
            } else {
                buckets.join(", ")
            },
            if point.parity() {
                "identical"
            } else {
                "MISMATCH"
            },
        );
    }
    out.push('\n');
}

/// The read evidence: what each leg's frames actually read, whether the settled
/// generation served that key, and what the dynamic fallback cost.
fn render_reads(bench: &Bench, out: &mut String) {
    let _ = write!(
        out,
        "## Candidate storage reads\n\n\
         `results` is how many candidates returned a result row (the only surface that\n\
         carries a post-execution access list), `reads` how many of those rows name the\n\
         read identity (`executor:0x5eed`) in that list. `served` says whether the\n\
         settled generation holds the key: `covered` serves it, `unknown` does not, and a\n\
         non-ready leg has no generation at all. `singles` is the dynamic-fallback count\n\
         the wire recorded for the fan-out.\n\n\
         | count | word | leg | simulated | results | reads | singles | served |\n\
         | --- | --- | --- | --- | --- | --- | --- | --- |\n"
    );
    for point in &bench.points {
        for leg in Leg::ALL {
            let Some(summary) = point.summary(leg) else {
                continue;
            };
            let served = match leg {
                Leg::ReadyCurrent | Leg::ReadyPrevious => "covered",
                Leg::ReadyCurrentUnknown => "unknown",
                _ => "n/a",
            };
            if summary.read_covered == 0 && leg.is_ready() {
                // Rendered as its own label so a coverage miss cannot read as a
                // covered control.
                let _ = writeln!(
                    out,
                    "| {} | {} | {} | {} | {} | {} | {} | NOT-SERVED |",
                    point.count,
                    point.word.label(),
                    leg.label(),
                    summary.candidates,
                    summary.result_rows,
                    summary.read_rows,
                    summary.singles,
                );
                continue;
            }
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} | {} |",
                point.count,
                point.word.label(),
                leg.label(),
                summary.candidates,
                summary.result_rows,
                summary.read_rows,
                summary.singles,
                served,
            );
        }
    }
    out.push('\n');
}

/// The ready-versus-legacy reduction table.
fn render_comparison(bench: &Bench, out: &mut String) {
    let _ = write!(
        out,
        "## Ready versus legacy\n\n\
         `singles` is the dynamic-fallback read count: the legacy lane pays one for the\n\
         corpus's own read identity (nothing derives that slot), the covered ready lane\n\
         pays none, and the unknown control's count is in its own table.\n\n\
         | count | word | model | legacy med us | ready-current med us | delta us | delta % | members legacy/ready | singles legacy/ready | packets legacy/ready | modeled us legacy/ready |\n\
         | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n"
    );
    for point in &bench.points {
        let (Some(off), Some(on)) = (
            point.summary(Leg::OffLegacy),
            point.summary(Leg::ReadyCurrent),
        ) else {
            continue;
        };
        let delta = i64::try_from(on.median).unwrap_or(i64::MAX)
            - i64::try_from(off.median).unwrap_or(i64::MAX);
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {}/{} | {}/{} | {}/{} | {}/{} |",
            point.count,
            point.word.label(),
            point.model.label(),
            off.median,
            on.median,
            delta,
            percent(delta, off.median),
            off.members,
            on.members,
            off.singles,
            on.singles,
            off.packets,
            on.packets,
            off.modeled,
            on.modeled,
        );
    }
    out.push('\n');
}

/// The cold controls: the fallback must be the legacy lane, BAL-free.
fn render_controls(bench: &Bench, out: &mut String) {
    let _ = write!(
        out,
        "## Cold controls\n\n\
         `bal` is the BAL fetch count on the DISPATCH leg: a control that prepared nothing\n\
         must report zero, and its batch must equal the legacy lane's.\n\n\
         | count | word | model | legacy members | unprepared members | undelivered members | unprepared bal | undelivered bal | unprepared lane | undelivered lane |\n\
         | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n"
    );
    for point in &bench.points {
        let (Some(off), Some(unprepared), Some(undelivered)) = (
            point.summary(Leg::OffLegacy),
            point.summary(Leg::ColdUnprepared),
            point.summary(Leg::ColdUndelivered),
        ) else {
            continue;
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            point.count,
            point.word.label(),
            point.model.label(),
            off.members,
            unprepared.members,
            undelivered.members,
            unprepared.bal,
            undelivered.bal,
            unprepared.lane,
            undelivered.lane,
        );
    }
    out.push('\n');
}

/// The background BAL cost, in its own table.
fn render_background(bench: &Bench, out: &mut String) {
    let _ = write!(
        out,
        "## Background BAL cost (never subtracted)\n\n\
         Per settled head: the prepare wall (list fetch + full decode) and the apply wall\n\
         (the one synchronous transition), one row for the covered list and one for the\n\
         unknown list so the single-entry difference is visible. `prefetch covered` is how\n\
         many identities the legacy lane's prefetch derived that the settled generation\n\
         answers with; `read key` says whether the generation also holds the candidate's\n\
         own read identity.\n\n\
         | count | word | model | list | heads | prepare median us | prepare max us | apply median us | apply max us | writes median | skipped median | prefetch covered | read key | hub entries |\n\
         | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n"
    );
    for point in &bench.points {
        for (leg, list) in [
            (Leg::ReadyCurrent, "covered"),
            (Leg::ReadyCurrentUnknown, "unknown"),
        ] {
            let runs = point.leg(leg);
            let generations: Vec<&Generation> = runs
                .iter()
                .filter_map(|run| run.generation.as_ref())
                .collect();
            let Some(first) = generations.first() else {
                continue;
            };
            let prepares: Vec<u64> = generations
                .iter()
                .flat_map(|generation| generation.prepare_us.iter().copied())
                .collect();
            let applies: Vec<u64> = generations
                .iter()
                .flat_map(|generation| generation.apply_us.iter().copied())
                .collect();
            let counts = |pick: fn(&Generation) -> &Vec<usize>| -> Vec<u64> {
                generations
                    .iter()
                    .flat_map(|generation| {
                        pick(generation)
                            .iter()
                            .map(|&n| u64::try_from(n).unwrap_or(0))
                    })
                    .collect()
            };
            let writes = counts(|generation| &generation.writes);
            let skipped = counts(|generation| &generation.writes_skipped);
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {}/{} | {} | {} |",
                point.count,
                point.word.label(),
                point.model.label(),
                list,
                first.anchors.len(),
                median(&prepares),
                prepares.iter().copied().max().unwrap_or(0),
                median(&applies),
                applies.iter().copied().max().unwrap_or(0),
                median(&writes),
                median(&skipped),
                first.identities_covered,
                first.identities_checked,
                if first.read_value.is_some() {
                    "covered"
                } else {
                    "absent"
                },
                first.hub_entries,
            );
        }
    }
    out.push('\n');
}

/// The variability table: what the medians hide.
fn render_variability(bench: &Bench, out: &mut String) {
    let _ = write!(
        out,
        "## Variability\n\n\
         The spread of each leg's repetitions at the largest count: a median far from the\n\
         maximum is an outlier the median hides.\n\n\
         | count | word | model | leg | median us | max us | max/median |\n\
         | --- | --- | --- | --- | --- | --- | --- |\n"
    );
    let largest = bench
        .points
        .iter()
        .map(|point| point.count)
        .max()
        .unwrap_or(0);
    for point in bench.points.iter().filter(|point| point.count == largest) {
        for leg in Leg::ALL {
            let Some(summary) = point.summary(leg) else {
                continue;
            };
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {:.2} |",
                point.count,
                point.word.label(),
                point.model.label(),
                leg.label(),
                summary.median,
                summary.max,
                spread(summary.median, summary.max),
            );
        }
    }
}

/// A wall's per-candidate share.
#[expect(clippy::cast_precision_loss)] // a microsecond wall over a candidate count
fn per_candidate(wall_us: u64, candidates: usize) -> f64 {
    if candidates == 0 {
        0.0
    } else {
        wall_us as f64 / candidates as f64
    }
}

/// How far the slowest repetition sits above the median one.
#[expect(clippy::cast_precision_loss)] // a wall ratio, printed to two places
fn spread(median_us: u64, max_us: u64) -> f64 {
    if median_us == 0 {
        0.0
    } else {
        max_us as f64 / median_us as f64
    }
}

/// A signed change as a percentage of `base`.
#[expect(clippy::cast_precision_loss)] // a wall delta as a share of one wall
fn percent(delta: i64, base: u64) -> String {
    if base == 0 {
        "n/a".to_string()
    } else {
        format!("{:+.1}%", delta as f64 * 100.0 / base as f64)
    }
}
